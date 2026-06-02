//! X224 (Slow-Path) PDU Processing
//!
//! This module handles PDUs received via the X224/T.125 data path, also known as
//! "slow-path" in RDP terminology. While modern RDP connections prefer fast-path
//! for graphics updates, slow-path is still used for:
//!
//! - Control PDUs (synchronize, control cooperate, etc.)
//! - Virtual channel data
//! - Legacy graphics updates (when fast-path is not negotiated)
//! - Deactivation/reactivation sequences
//!
//! # Slow-Path Graphics Updates
//!
//! When graphics are sent via slow-path, they arrive as [`ShareDataPdu::Update`]
//! containing [`ServerGraphicsUpdate`] data. The processor parses these and emits
//! appropriate [`ProcessorOutput`] variants:
//!
//! - [`ProcessorOutput::SlowPathBitmap`] - Bitmap image data
//! - [`ProcessorOutput::SlowPathOrders`] - GDI drawing commands
//! - [`ProcessorOutput::PaletteUpdate`] - Color palette for 8bpp mode
//! - [`ProcessorOutput::SlowPathPointer`] - Pointer shape/position data
//!
//! These are then routed to the fast-path processor for rendering, since the
//! actual drawing code is shared between both paths.
//!
//! [`ShareDataPdu::Update`]: ironrdp_pdu::rdp::headers::ShareDataPdu::Update
//! [`ServerGraphicsUpdate`]: ironrdp_pdu::rdp::server_graphics_update::ServerGraphicsUpdate

use ironrdp_core::{WriteBuf, decode, ReadCursor};
use ironrdp_dvc::{DrdynvcClient, DvcProcessor, DynamicVirtualChannel};
use ironrdp_pdu::mcs::{DisconnectProviderUltimatum, DisconnectReason, McsMessage, SendDataIndicationCtx};
use ironrdp_pdu::rdp::autodetect::{AutoDetectReqPdu, AutoDetectRequest, AutoDetectResponse, AutoDetectRspPdu};
use ironrdp_pdu::rdp::headers::ShareDataPdu;
use ironrdp_pdu::rdp::multitransport::MultitransportRequestPdu;
use ironrdp_pdu::rdp::server_error_info::{ErrorInfo, ProtocolIndependentCode, ServerSetErrorInfoPdu};
use ironrdp_pdu::rdp::server_graphics_update::ServerGraphicsUpdate;
use ironrdp_pdu::x224::X224;
use ironrdp_svc::{client_encode_svc_messages, StaticChannelSet, SvcMessage, SvcProcessor, SvcProcessorMessages};
use tracing::{debug, warn};

use crate::{SessionError, SessionErrorExt as _, SessionResult, reason_err};

/// X224 Processor output
#[derive(Debug, Clone)]
pub enum ProcessorOutput {
    /// A buffer with encoded data to send to the server.
    ResponseFrame(Vec<u8>),
    /// A graceful disconnect notification. Client should close the connection upon receiving this.
    Disconnect(DisconnectDescription),
    /// Received a [`ironrdp_pdu::rdp::headers::ServerDeactivateAll`] PDU. Client should execute the
    /// [Deactivation-Reactivation Sequence].
    ///
    /// [Deactivation-Reactivation Sequence]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/dfc234ce-481a-4674-9a5d-2a7bafb14432
    DeactivateAll,
    /// Server Initiate Multitransport Request. The application should establish a
    /// sideband UDP transport using the request ID and security cookie, then send
    /// a [`MultitransportResponsePdu`] back on the IO channel.
    ///
    /// See [\[MS-RDPBCGR\] 2.2.15.1].
    ///
    /// [\[MS-RDPBCGR\] 2.2.15.1]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/de783158-8b01-4818-8fb0-62523a5b3490
    /// [`MultitransportResponsePdu`]: ironrdp_pdu::rdp::multitransport::MultitransportResponsePdu
    MultitransportRequest(MultitransportRequestPdu),
    /// Auto-detect network characteristics from server ([\[MS-RDPBCGR\] 2.2.14]).
    ///
    /// Currently only surfaces [`AutoDetectRequest::NetworkCharacteristicsResult`].
    /// RTT requests are handled internally with automatic responses.
    ///
    /// [\[MS-RDPBCGR\] 2.2.14]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/dc672839-4f4e-40b1-a71c-cd6a959baa38
    AutoDetect(AutoDetectRequest),
    /// Slow-path bitmap update - route to fast_path processor for rendering.
    SlowPathBitmap(Vec<u8>),
    /// Palette update for 8bpp color mode.
    PaletteUpdate(ironrdp_pdu::rdp::server_graphics_update::PaletteUpdate),
    /// Slow-path drawing orders update - route to fast_path processor for rendering.
    SlowPathOrders {
        number_orders: u16,
        order_data: Vec<u8>,
    },
    /// Slow-path pointer update ([MS-RDPBCGR] 2.2.9.1.1.4).
    SlowPathPointer(Vec<u8>),
}

#[derive(Debug, Clone)]
pub enum DisconnectDescription {
    /// Includes the reason from the MCS Disconnect Provider Ultimatum.
    /// This is the least-specific disconnect reason and is only used
    /// when a more specific disconnect code is not available.
    McsDisconnect(DisconnectReason),

    /// Includes the error information sent by the RDP server when there
    /// is a connection or disconnection failure.
    ErrorInfo(ErrorInfo),
}

pub struct Processor {
    static_channels: StaticChannelSet,
    user_channel_id: u16,
    io_channel_id: u16,
    message_channel_id: Option<u16>,
    share_id: u32,
}

impl Processor {
    pub fn new(
        static_channels: StaticChannelSet,
        user_channel_id: u16,
        io_channel_id: u16,
        message_channel_id: Option<u16>,
        share_id: u32,
    ) -> Self {
        Self {
            static_channels,
            user_channel_id,
            io_channel_id,
            message_channel_id,
            share_id,
        }
    }

    pub fn set_share_id(&mut self, share_id: u32) {
        self.share_id = share_id;
    }

    pub fn get_svc_processor<T: SvcProcessor + 'static>(&self) -> Option<&T> {
        self.static_channels
            .get_by_type::<T>()
            .and_then(|svc| svc.channel_processor_downcast_ref())
    }

    pub fn get_svc_processor_mut<T: SvcProcessor + 'static>(&mut self) -> Option<&mut T> {
        self.static_channels
            .get_by_type_mut::<T>()
            .and_then(|svc| svc.channel_processor_downcast_mut())
    }

    /// Completes user's SVC request with data, required to sent it over the network and returns
    /// a buffer with encoded data.
    pub fn process_svc_processor_messages<C: SvcProcessor + 'static>(
        &self,
        messages: SvcProcessorMessages<C>,
    ) -> SessionResult<Vec<u8>> {
        let channel_id = self
            .static_channels
            .get_channel_id_by_type::<C>()
            .ok_or_else(|| reason_err!("SVC", "channel not found"))?;

        process_svc_messages(messages.into(), channel_id, self.user_channel_id)
    }

    pub fn get_dvc<T: DvcProcessor + 'static>(&self) -> Option<&DynamicVirtualChannel> {
        self.get_svc_processor::<DrdynvcClient>()?.get_dvc_by_type_id::<T>()
    }

    pub fn get_dvc_by_channel_id(&self, channel_id: u32) -> Option<&DynamicVirtualChannel> {
        self.get_svc_processor::<DrdynvcClient>()?
            .get_dvc_by_channel_id(channel_id)
    }

    /// Processes a received PDU. Returns a vector of [`ProcessorOutput`] that must be processed
    /// in the returned order.
    pub fn process(&mut self, frame: &[u8]) -> SessionResult<Vec<ProcessorOutput>> {
        // Peek at the MCS message type first. xrdp and some Windows servers send
        // DisconnectProviderUltimatum when the session ends; without this check,
        // decode_send_data_indication returns an error for any non-SendDataIndication
        // message, which surfaces as a confusing "general error" instead of a clean
        // session termination.
        if let Ok(mcs_msg) = decode::<X224<McsMessage<'_>>>(frame) {
            if let McsMessage::DisconnectProviderUltimatum(msg) = mcs_msg.0 {
                return Ok(vec![ProcessorOutput::Disconnect(
                    DisconnectDescription::McsDisconnect(msg.reason),
                )]);
            }
        }

        let data_ctx: SendDataIndicationCtx<'_> =
            ironrdp_pdu::mcs::decode_send_data_indication(frame).map_err(SessionError::decode)?;
        let channel_id = data_ctx.channel_id;

        if channel_id == self.io_channel_id {
            self.process_io_channel(data_ctx)
        } else if self.message_channel_id == Some(channel_id) {
            self.process_message_channel(data_ctx)
        } else if let Some(svc) = self.static_channels.get_by_channel_id_mut(channel_id) {
            let response_pdus = svc.process(data_ctx.user_data).map_err(SessionError::pdu)?;
            process_svc_messages(response_pdus, channel_id, data_ctx.initiator_id)
                .map(|data| vec![ProcessorOutput::ResponseFrame(data)])
        } else {
            Err(reason_err!("X224", "unexpected channel received: ID {channel_id}"))
        }
    }

    fn process_io_channel(&self, data_ctx: SendDataIndicationCtx<'_>) -> SessionResult<Vec<ProcessorOutput>> {
        debug_assert_eq!(data_ctx.channel_id, self.io_channel_id);

        let io_channel = ironrdp_pdu::rdp::headers::decode_io_channel(data_ctx).map_err(SessionError::decode)?;

        match io_channel {
            ironrdp_pdu::rdp::headers::IoChannelPdu::Data(ctx) => {
                match ctx.pdu {
                    ShareDataPdu::SaveSessionInfo(session_info) => {
                        debug!("Got Session Save Info PDU: {session_info:?}");
                        Ok(Vec::new())
                    }
                    // FIXME: workaround fix to not terminate the session on "unhandled PDU: Set Keyboard Indicators PDU"
                    ShareDataPdu::SetKeyboardIndicators(data) => {
                        debug!("Got Keyboard Indicators PDU: {data:?}");
                        Ok(Vec::new())
                    }
                    // Handle slow-path Update PDU (MS-RDPBCGR 2.2.9.1.1.3)
                    ShareDataPdu::Update(data) => {
                        // Parse the slow-path Update PDU
                        let mut cursor = ReadCursor::new(&data);
                        match ServerGraphicsUpdate::decode(&mut cursor) {
                            Ok(ServerGraphicsUpdate::Bitmap(bitmap_data)) => {
                                // Return bitmap data for processing by fast_path processor
                                // We pass the raw data so it can be processed uniformly
                                debug!("Got Bitmap Update PDU: {} rectangles", bitmap_data.rectangles.len());
                                Ok(vec![ProcessorOutput::SlowPathBitmap(data)])
                            }
                            Ok(ServerGraphicsUpdate::Orders(orders)) => {
                                // Pass orders to fast_path processor for rendering
                                debug!("Got Orders Update PDU: {} orders", orders.number_orders);
                                Ok(vec![ProcessorOutput::SlowPathOrders {
                                    number_orders: orders.number_orders,
                                    order_data: orders.order_data.to_vec(),
                                }])
                            }
                            Ok(ServerGraphicsUpdate::Palette(palette)) => {
                                // Palette updates for 8bpp mode - pass to fast_path processor
                                debug!("Got Palette Update PDU: {} entries", palette.entries.len());
                                Ok(vec![ProcessorOutput::PaletteUpdate(palette)])
                            }
                            Ok(ServerGraphicsUpdate::Synchronize) => {
                                debug!("Got Synchronize Update PDU");
                                Ok(Vec::new())
                            }
                            Err(e) => {
                                warn!("Failed to parse Update PDU: {}", e);
                                Ok(Vec::new())
                            }
                        }
                    }
                    ShareDataPdu::ServerSetErrorInfo(ServerSetErrorInfoPdu(ErrorInfo::ProtocolIndependentCode(
                        ProtocolIndependentCode::None,
                    ))) => {
                        debug!("Received None server error");
                        Ok(Vec::new())
                    }
                    ShareDataPdu::ServerSetErrorInfo(ServerSetErrorInfoPdu(e)) => {
                        // This is a part of server-side graceful disconnect procedure defined
                        // in [MS-RDPBCGR].
                        //
                        // [MS-RDPBCGR]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/149070b0-ecec-4c20-af03-934bbc48adb8
                        let desc = DisconnectDescription::ErrorInfo(e);
                        Ok(vec![ProcessorOutput::Disconnect(desc)])
                    }
                    ShareDataPdu::ShutdownDenied => {
                        debug!("ShutdownDenied received, session will be closed");

                        // As defined in [MS-RDPBCGR], when `ShareDataPdu::ShutdownDenied` is received, we
                        // need to send a disconnect ultimatum to the server if we want to proceed with the
                        // session shutdown.
                        //
                        // [MS-RDPBCGR]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/27915739-8f77-487e-9927-55008af7fd68
                        let ultimatum = McsMessage::DisconnectProviderUltimatum(
                            DisconnectProviderUltimatum::from_reason(DisconnectReason::UserRequested),
                        );

                        let encoded_pdu = ironrdp_core::encode_vec(&X224(ultimatum)).map_err(SessionError::encode);

                        Ok(vec![
                            ProcessorOutput::ResponseFrame(encoded_pdu?),
                            ProcessorOutput::Disconnect(DisconnectDescription::McsDisconnect(
                                DisconnectReason::UserRequested,
                            )),
                        ])
                    }
                    ShareDataPdu::Pointer(data) => {
                        debug!("Got slow-path pointer update ({} bytes)", data.len());
                        Ok(vec![ProcessorOutput::SlowPathPointer(data)])
                    }
                    _ => Err(reason_err!(
                        "IO channel",
                        "unhandled PDU: {:?}",
                        ctx.pdu.as_short_name()
                    )),
                }
            }
            ironrdp_pdu::rdp::headers::IoChannelPdu::MultitransportRequest(pdu) => {
                debug!(
                    "Received Initiate Multitransport Request: request_id={}",
                    pdu.request_id
                );
                Ok(vec![ProcessorOutput::MultitransportRequest(pdu)])
            }
            ironrdp_pdu::rdp::headers::IoChannelPdu::DeactivateAll(_) => Ok(vec![ProcessorOutput::DeactivateAll]),
        }
    }

    /// Process an auto-detect request received on the MCS message channel.
    ///
    /// During continuous auto-detection ([MS-RDPBCGR] 2.2.14) the server sends
    /// RTT (and bandwidth) requests on the message channel; the client answers
    /// RTT requests and surfaces the final Network Characteristics Result.
    fn process_message_channel(&self, data_ctx: SendDataIndicationCtx<'_>) -> SessionResult<Vec<ProcessorOutput>> {
        let Some(message_channel_id) = self.message_channel_id else {
            return Err(reason_err!("message channel", "no message channel negotiated"));
        };

        let req = decode::<AutoDetectReqPdu>(data_ctx.user_data).map_err(SessionError::decode)?;

        match req.request {
            AutoDetectRequest::RttRequest { sequence_number, .. } => {
                let response = AutoDetectRspPdu::new(AutoDetectResponse::RttResponse { sequence_number });
                let mut frame = WriteBuf::new();
                ironrdp_pdu::mcs::encode_send_data_request(
                    self.user_channel_id,
                    message_channel_id,
                    &response,
                    &mut frame,
                )
                .map_err(SessionError::encode)?;
                debug!(sequence_number, "Responded to auto-detect RTT request");
                Ok(vec![ProcessorOutput::ResponseFrame(frame.into_inner())])
            }
            req @ AutoDetectRequest::NetworkCharacteristicsResult { .. } => {
                debug!(?req, "Received network characteristics from server");
                Ok(vec![ProcessorOutput::AutoDetect(req)])
            }
            req => {
                debug!(?req, "Auto-detect request not yet implemented");
                Ok(Vec::new())
            }
        }
    }

    /// Send a pdu on the static global channel. Typically used to send input events
    pub fn encode_static(&self, output: &mut WriteBuf, pdu: ShareDataPdu) -> SessionResult<usize> {
        let written = ironrdp_pdu::rdp::headers::encode_share_data(
            self.user_channel_id,
            self.io_channel_id,
            self.share_id,
            pdu,
            output,
        )
        .map_err(SessionError::encode)?;
        Ok(written)
    }
}

/// Processes a vector of [`SvcMessage`] in preparation for sending them to the server on the `channel_id` channel.
///
/// This includes chunkifying the messages, adding MCS, x224, and tpkt headers, and encoding them into a buffer.
/// The messages returned here are ready to be sent to the server.
///
/// The caller is responsible for ensuring that the `channel_id` corresponds to the correct channel.
fn process_svc_messages(messages: Vec<SvcMessage>, channel_id: u16, initiator_id: u16) -> SessionResult<Vec<u8>> {
    client_encode_svc_messages(messages, channel_id, initiator_id).map_err(SessionError::encode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_pdu::mcs::{DisconnectProviderUltimatum, DisconnectReason, McsMessage};

    /// Verify that a DisconnectProviderUltimatum PDU can be encoded and that the
    /// decode path used by Processor::process() correctly identifies it.
    ///
    /// Before this fix, decode_send_data_indication returned an error for any
    /// non-SendDataIndication message, causing the session to crash with a
    /// confusing "general error" instead of a clean termination. Now we peek
    /// at the MCS message type first and return ProcessorOutput::Disconnect.
    #[test]
    fn disconnect_provider_ultimatum_decoded_as_disconnect_output() {
        // Encode a DisconnectProviderUltimatum wrapped in X224 — the exact format
        // that xrdp and Windows servers send when closing a session.
        let ultimatum = McsMessage::DisconnectProviderUltimatum(
            DisconnectProviderUltimatum::from_reason(DisconnectReason::ProviderInitiated),
        );
        let frame = ironrdp_core::encode_vec(&X224(ultimatum))
            .expect("failed to encode DisconnectProviderUltimatum");

        // Our fix: peek at the MCS message type before calling
        // decode_send_data_indication, which would return an error here.
        let mcs_msg = decode::<X224<McsMessage<'_>>>(&frame)
            .expect("failed to decode X224<McsMessage>");

        match mcs_msg.0 {
            McsMessage::DisconnectProviderUltimatum(msg) => {
                assert_eq!(msg.reason, DisconnectReason::ProviderInitiated);
                // Verify this maps to the correct ProcessorOutput
                let output = ProcessorOutput::Disconnect(DisconnectDescription::McsDisconnect(msg.reason));
                match output {
                    ProcessorOutput::Disconnect(DisconnectDescription::McsDisconnect(
                        DisconnectReason::ProviderInitiated,
                    )) => {} // correct
                    other => panic!("unexpected output: {:?}", other),
                }
            }
            other => panic!("expected DisconnectProviderUltimatum, got unexpected MCS message: {:?}", other),
        }
    }

    /// Verify that the fix handles UserRequested reason as well.
    #[test]
    fn disconnect_user_requested_decoded_correctly() {
        let ultimatum = McsMessage::DisconnectProviderUltimatum(
            DisconnectProviderUltimatum::from_reason(DisconnectReason::UserRequested),
        );
        let frame = ironrdp_core::encode_vec(&X224(ultimatum))
            .expect("failed to encode DisconnectProviderUltimatum");

        let mcs_msg = decode::<X224<McsMessage<'_>>>(&frame)
            .expect("failed to decode");

        assert!(matches!(
            mcs_msg.0,
            McsMessage::DisconnectProviderUltimatum(DisconnectProviderUltimatum {
                reason: DisconnectReason::UserRequested,
            })
        ));
    }
}
