use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::fs::MetadataExt;

use ironrdp_core::impl_as_any;
use ironrdp_pdu::{encode_err, PduResult};
use ironrdp_rdpdr::pdu::efs::*;
use ironrdp_rdpdr::pdu::esc::{ScardCall, ScardIoCtlCode};
use ironrdp_rdpdr::pdu::RdpdrPdu;
use ironrdp_rdpdr::RdpdrBackend;
use ironrdp_svc::SvcMessage;
use tracing::{debug, warn};

#[derive(Debug, Default)]
pub struct WinRdpdrBackend {
    file_id: u32,
    file_base: String,
    file_map: std::collections::HashMap<u32, std::fs::File>,
    file_path_map: std::collections::HashMap<u32, String>,
    // ReadDir iterator for in-progress directory listings
    file_dir_map: std::collections::HashMap<u32, std::fs::ReadDir>,
}

impl WinRdpdrBackend {
    pub fn new(file_base: String) -> Self {
        Self {
            file_base,
            ..Default::default()
        }
    }
}

impl_as_any!(WinRdpdrBackend);

impl RdpdrBackend for WinRdpdrBackend {
    fn handle_server_device_announce_response(&mut self, _pdu: ServerDeviceAnnounceResponse) -> PduResult<()> {
        Ok(())
    }

    fn handle_scard_call(
        &mut self,
        _req: DeviceControlRequest<ScardIoCtlCode>,
        _call: ScardCall,
    ) -> PduResult<()> {
        Ok(())
    }

    fn handle_drive_io_request(&mut self, req: ServerDriveIoRequest) -> PduResult<Vec<SvcMessage>> {
        debug!("handle_drive_io_request:{:?}", req);
        match req {
            ServerDriveIoRequest::DeviceWriteRequest(req_inner) => write_device(self, req_inner),
            ServerDriveIoRequest::ServerCreateDriveRequest(req_inner) => create_drive(self, req_inner),
            ServerDriveIoRequest::DeviceReadRequest(req_inner) => read_device(self, req_inner),
            ServerDriveIoRequest::DeviceCloseRequest(req_inner) => close_device(self, req_inner),
            ServerDriveIoRequest::ServerDriveNotifyChangeDirectoryRequest(_) => {
                // TODO
                Ok(Vec::new())
            }
            ServerDriveIoRequest::ServerDriveQueryDirectoryRequest(req_inner) => {
                query_directory(self, req_inner)
            }
            ServerDriveIoRequest::ServerDriveQueryInformationRequest(req_inner) => {
                query_information(self, req_inner)
            }
            ServerDriveIoRequest::ServerDriveQueryVolumeInformationRequest(req_inner) => {
                query_volume_information(self, req_inner)
            }
            ServerDriveIoRequest::ServerDriveSetInformationRequest(req_inner) => {
                set_information(self, req_inner)
            }
            ServerDriveIoRequest::DeviceControlRequest(req_inner) => {
                Ok(vec![SvcMessage::from(RdpdrPdu::DeviceControlResponse(
                    DeviceControlResponse {
                        device_io_reply: DeviceIoResponse::new(req_inner.header, NtStatus::SUCCESS),
                        output_buffer: None,
                    },
                ))])
            }
            ServerDriveIoRequest::ServerDriveLockControlRequest(_) => {
                // TODO
                Ok(Vec::new())
            }
        }
    }
}

fn write_device(backend: &mut WinRdpdrBackend, req_inner: DeviceWriteRequest) -> PduResult<Vec<SvcMessage>> {
    return process_dependent_file(
        backend,
        req_inner.device_io_request,
        |request| {
            Ok(vec![SvcMessage::from(RdpdrPdu::DeviceWriteResponse(
                DeviceWriteResponse {
                    device_io_reply: DeviceIoResponse::new(request, NtStatus::NO_SUCH_FILE),
                    length: 0u32,
                },
            ))])
        },
        |file, request| match write_inner(file, req_inner.offset, &req_inner.write_data) {
            Ok(length) => {
                if length == req_inner.write_data.len() {
                    Ok(vec![SvcMessage::from(RdpdrPdu::DeviceWriteResponse(
                        DeviceWriteResponse {
                            device_io_reply: DeviceIoResponse::new(request, NtStatus::SUCCESS),
                            length: u32::try_from(req_inner.write_data.len()).unwrap(),
                        },
                    ))])
                } else {
                    warn!(
                        "Written content len:{} is not equal to {}",
                        length,
                        req_inner.write_data.len()
                    );
                    Ok(vec![SvcMessage::from(RdpdrPdu::DeviceWriteResponse(
                        DeviceWriteResponse {
                            device_io_reply: DeviceIoResponse::new(request, NtStatus::UNSUCCESSFUL),
                            length: 0u32,
                        },
                    ))])
                }
            }
            Err(error) => {
                warn!(%error, "Write error");
                Ok(vec![SvcMessage::from(RdpdrPdu::DeviceWriteResponse(
                    DeviceWriteResponse {
                        device_io_reply: DeviceIoResponse::new(request, NtStatus::UNSUCCESSFUL),
                        length: 0u32,
                    },
                ))])
            }
        },
    );

    fn write_inner(file: &mut std::fs::File, offset: u64, write_data: &[u8]) -> std::io::Result<usize> {
        file.seek(SeekFrom::Start(offset))?;
        let length = file.write(write_data)?;
        file.flush()?;
        Ok(length)
    }
}

fn read_device(backend: &mut WinRdpdrBackend, req_inner: DeviceReadRequest) -> PduResult<Vec<SvcMessage>> {
    return process_dependent_file(
        backend,
        req_inner.device_io_request,
        |request| {
            Ok(vec![SvcMessage::from(RdpdrPdu::DeviceReadResponse(
                DeviceReadResponse {
                    device_io_reply: DeviceIoResponse::new(request, NtStatus::NO_SUCH_FILE),
                    read_data: Vec::new(),
                },
            ))])
        },
        |file, request| {
            match read_inner(file, req_inner.offset, usize::try_from(req_inner.length).unwrap()) {
                Ok(buf) => Ok(vec![SvcMessage::from(RdpdrPdu::DeviceReadResponse(
                    DeviceReadResponse {
                        device_io_reply: DeviceIoResponse::new(request, NtStatus::SUCCESS),
                        read_data: buf,
                    },
                ))]),
                Err(error) => {
                    warn!(?error, "Read error");
                    Ok(vec![SvcMessage::from(RdpdrPdu::DeviceReadResponse(
                        DeviceReadResponse {
                            device_io_reply: DeviceIoResponse::new(request, NtStatus::UNSUCCESSFUL),
                            read_data: Vec::new(),
                        },
                    ))])
                }
            }
        },
    );

    fn read_inner(file: &mut std::fs::File, offset: u64, length: usize) -> std::io::Result<Vec<u8>> {
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; length];
        let n = file.read(&mut buf)?;
        buf.resize(n, 0u8);
        Ok(buf)
    }
}

fn close_device(backend: &mut WinRdpdrBackend, req_inner: DeviceCloseRequest) -> PduResult<Vec<SvcMessage>> {
    backend.file_map.remove(&req_inner.device_io_request.file_id);
    backend.file_path_map.remove(&req_inner.device_io_request.file_id);
    backend.file_dir_map.remove(&req_inner.device_io_request.file_id);
    Ok(vec![SvcMessage::from(RdpdrPdu::DeviceCloseResponse(
        DeviceCloseResponse {
            device_io_response: DeviceIoResponse::new(req_inner.device_io_request, NtStatus::SUCCESS),
        },
    ))])
}

fn query_information(
    backend: &mut WinRdpdrBackend,
    req_inner: ServerDriveQueryInformationRequest,
) -> PduResult<Vec<SvcMessage>> {
    match backend.file_map.get(&req_inner.device_io_request.file_id) {
        Some(file) => match file.metadata() {
            Ok(meta) => {
                let path = backend
                    .file_path_map
                    .get(&req_inner.device_io_request.file_id)
                    .cloned()
                    .unwrap_or_default();
                #[expect(clippy::arithmetic_side_effects)]
                let name_index = match path.rfind('/').or_else(|| path.rfind('\\')) {
                    Some(index) => index + 1,
                    None => 0,
                };
                let name = &path[name_index..];
                let file_attribute = get_file_attributes(&meta, name);

                if FileInformationClassLevel::FILE_BASIC_INFORMATION == req_inner.file_info_class_lvl {
                    let basic_info = FileBasicInformation {
                        creation_time: i64::try_from(meta.creation_time()).unwrap_or(0),
                        last_access_time: i64::try_from(meta.last_access_time()).unwrap_or(0),
                        last_write_time: i64::try_from(meta.last_write_time()).unwrap_or(0),
                        change_time: i64::try_from(meta.last_write_time()).unwrap_or(0),
                        file_attributes: file_attribute,
                    };
                    Ok(vec![SvcMessage::from(
                        RdpdrPdu::ClientDriveQueryInformationResponse(
                            ClientDriveQueryInformationResponse {
                                device_io_response: DeviceIoResponse::new(
                                    req_inner.device_io_request,
                                    NtStatus::SUCCESS,
                                ),
                                buffer: Some(FileInformationClass::Basic(basic_info)),
                            },
                        ),
                    )])
                } else if FileInformationClassLevel::FILE_STANDARD_INFORMATION
                    == req_inner.file_info_class_lvl
                {
                    let dir = if meta.is_dir() { Boolean::True } else { Boolean::False };
                    let standard_info = FileStandardInformation {
                        allocation_size: i64::try_from(meta.len()).unwrap_or(0),
                        end_of_file: i64::try_from(meta.len()).unwrap_or(0),
                        number_of_links: 1u32,
                        delete_pending: Boolean::False,
                        directory: dir,
                    };
                    Ok(vec![SvcMessage::from(
                        RdpdrPdu::ClientDriveQueryInformationResponse(
                            ClientDriveQueryInformationResponse {
                                device_io_response: DeviceIoResponse::new(
                                    req_inner.device_io_request,
                                    NtStatus::SUCCESS,
                                ),
                                buffer: Some(FileInformationClass::Standard(standard_info)),
                            },
                        ),
                    )])
                } else if FileInformationClassLevel::FILE_ATTRIBUTE_TAG_INFORMATION
                    == req_inner.file_info_class_lvl
                {
                    let info = FileAttributeTagInformation {
                        file_attributes: file_attribute,
                        reparse_tag: 0,
                    };
                    Ok(vec![SvcMessage::from(
                        RdpdrPdu::ClientDriveQueryInformationResponse(
                            ClientDriveQueryInformationResponse {
                                device_io_response: DeviceIoResponse::new(
                                    req_inner.device_io_request,
                                    NtStatus::SUCCESS,
                                ),
                                buffer: Some(FileInformationClass::AttributeTag(info)),
                            },
                        ),
                    )])
                } else {
                    warn!("unsupported file class");
                    Ok(vec![SvcMessage::from(
                        RdpdrPdu::ClientDriveQueryInformationResponse(
                            ClientDriveQueryInformationResponse {
                                device_io_response: DeviceIoResponse::new(
                                    req_inner.device_io_request,
                                    NtStatus::UNSUCCESSFUL,
                                ),
                                buffer: None,
                            },
                        ),
                    )])
                }
            }
            Err(error) => {
                warn!(?error, "Get file metadata error");
                Ok(vec![SvcMessage::from(
                    RdpdrPdu::ClientDriveQueryInformationResponse(
                        ClientDriveQueryInformationResponse {
                            device_io_response: DeviceIoResponse::new(
                                req_inner.device_io_request,
                                NtStatus::UNSUCCESSFUL,
                            ),
                            buffer: None,
                        },
                    ),
                )])
            }
        },
        None => {
            warn!("no such file");
            Ok(vec![SvcMessage::from(
                RdpdrPdu::ClientDriveQueryInformationResponse(
                    ClientDriveQueryInformationResponse {
                        device_io_response: DeviceIoResponse::new(
                            req_inner.device_io_request,
                            NtStatus::NO_SUCH_FILE,
                        ),
                        buffer: None,
                    },
                ),
            )])
        }
    }
}

fn query_volume_information(
    backend: &mut WinRdpdrBackend,
    req_inner: ServerDriveQueryVolumeInformationRequest,
) -> PduResult<Vec<SvcMessage>> {
    if backend.file_map.get(&req_inner.device_io_request.file_id).is_none() {
        warn!("no such file for volume query");
        return Ok(vec![SvcMessage::from(
            RdpdrPdu::ClientDriveQueryInformationResponse(ClientDriveQueryInformationResponse {
                device_io_response: DeviceIoResponse::new(
                    req_inner.device_io_request,
                    NtStatus::NO_SUCH_FILE,
                ),
                buffer: None,
            }),
        )]);
    }

    // Use fixed reasonable disk space values (1TB total, 500GB free).
    // Exact values are not meaningful for the shared drive use case.
    let block_size: u32 = 4096;
    let total_blocks: i64 = 256 * 1024 * 1024; // 1 TiB / 4096
    let free_blocks: i64 = 128 * 1024 * 1024; // 512 GiB / 4096

    if FileSystemInformationClassLevel::FILE_FS_FULL_SIZE_INFORMATION == req_inner.fs_info_class_lvl {
        let info = FileFsFullSizeInformation {
            total_alloc_units: total_blocks,
            caller_available_alloc_units: free_blocks,
            actual_available_alloc_units: free_blocks,
            sectors_per_alloc_unit: block_size,
            bytes_per_sector: 1,
        };
        Ok(vec![SvcMessage::from(
            RdpdrPdu::ClientDriveQueryVolumeInformationResponse(
                ClientDriveQueryVolumeInformationResponse {
                    device_io_reply: DeviceIoResponse::new(req_inner.device_io_request, NtStatus::SUCCESS),
                    buffer: Some(FileSystemInformationClass::FileFsFullSizeInformation(info)),
                },
            ),
        )])
    } else if FileSystemInformationClassLevel::FILE_FS_SIZE_INFORMATION == req_inner.fs_info_class_lvl {
        let info = FileFsSizeInformation {
            total_alloc_units: total_blocks,
            available_alloc_units: free_blocks,
            sectors_per_alloc_unit: block_size,
            bytes_per_sector: 1,
        };
        Ok(vec![SvcMessage::from(
            RdpdrPdu::ClientDriveQueryVolumeInformationResponse(
                ClientDriveQueryVolumeInformationResponse {
                    device_io_reply: DeviceIoResponse::new(req_inner.device_io_request, NtStatus::SUCCESS),
                    buffer: Some(FileSystemInformationClass::FileFsSizeInformation(info)),
                },
            ),
        )])
    } else if FileSystemInformationClassLevel::FILE_FS_ATTRIBUTE_INFORMATION == req_inner.fs_info_class_lvl {
        Ok(vec![SvcMessage::from(
            RdpdrPdu::ClientDriveQueryVolumeInformationResponse(
                ClientDriveQueryVolumeInformationResponse {
                    device_io_reply: DeviceIoResponse::new(req_inner.device_io_request, NtStatus::SUCCESS),
                    buffer: Some(FileSystemInformationClass::FileFsAttributeInformation(
                        FileFsAttributeInformation {
                            file_system_attributes: FileSystemAttributes::FILE_CASE_PRESERVED_NAMES
                                | FileSystemAttributes::FILE_UNICODE_ON_DISK,
                            max_component_name_len: 260,
                            file_system_name: "NTFS".to_owned(),
                        },
                    )),
                },
            ),
        )])
    } else if FileSystemInformationClassLevel::FILE_FS_VOLUME_INFORMATION == req_inner.fs_info_class_lvl {
        Ok(vec![SvcMessage::from(
            RdpdrPdu::ClientDriveQueryVolumeInformationResponse(
                ClientDriveQueryVolumeInformationResponse {
                    device_io_reply: DeviceIoResponse::new(req_inner.device_io_request, NtStatus::SUCCESS),
                    buffer: Some(FileSystemInformationClass::FileFsVolumeInformation(
                        FileFsVolumeInformation {
                            volume_creation_time: 0,
                            volume_serial_number: 0x4b504552, // "KPER"
                            supports_objects: Boolean::False,
                            volume_label: "KEEPER_DRIVE".to_owned(),
                        },
                    )),
                },
            ),
        )])
    } else {
        warn!("unsupported volume class");
        Ok(vec![SvcMessage::from(
            RdpdrPdu::ClientDriveQueryVolumeInformationResponse(
                ClientDriveQueryVolumeInformationResponse {
                    device_io_reply: DeviceIoResponse::new(
                        req_inner.device_io_request,
                        NtStatus::UNSUCCESSFUL,
                    ),
                    buffer: None,
                },
            ),
        )])
    }
}

fn set_information(
    backend: &mut WinRdpdrBackend,
    req_inner: ServerDriveSetInformationRequest,
) -> PduResult<Vec<SvcMessage>> {
    match backend.file_path_map.get(&req_inner.device_io_request.file_id) {
        Some(file) => {
            match &req_inner.set_buffer {
                FileInformationClass::Rename(info) => {
                    let mut to = backend.file_base.clone();
                    to.push_str(&info.file_name.replace('\\', "/"));
                    if let Err(error) = std::fs::rename(file, to) {
                        warn!(?error, "Rename file error");
                        let res = RdpdrPdu::ClientDriveSetInformationResponse(
                            ClientDriveSetInformationResponse::new(&req_inner, NtStatus::UNSUCCESSFUL)
                                .map_err(|e| encode_err!(e))?,
                        );
                        return Ok(vec![SvcMessage::from(res)]);
                    }
                }
                FileInformationClass::Allocation(_) => {
                    // nothing to do
                }
                FileInformationClass::Disposition(_) => {
                    if let Err(error) = std::fs::remove_file(file) {
                        warn!(?error, "Remove file error");
                        let res = RdpdrPdu::ClientDriveSetInformationResponse(
                            ClientDriveSetInformationResponse::new(&req_inner, NtStatus::UNSUCCESSFUL)
                                .map_err(|e| encode_err!(e))?,
                        );
                        return Ok(vec![SvcMessage::from(res)]);
                    }
                }
                FileInformationClass::EndOfFile(info) => {
                    if let Some(file) = backend.file_map.get(&req_inner.device_io_request.file_id) {
                        if let Err(error) = file.set_len(u64::try_from(info.end_of_file).unwrap_or(0)) {
                            warn!(%error, "Failed to set end of file");
                            let res = RdpdrPdu::ClientDriveSetInformationResponse(
                                ClientDriveSetInformationResponse::new(&req_inner, NtStatus::UNSUCCESSFUL)
                                    .map_err(|e| encode_err!(e))?,
                            );
                            return Ok(vec![SvcMessage::from(res)]);
                        }
                    } else {
                        warn!("no such file");
                        let res = RdpdrPdu::ClientDriveSetInformationResponse(
                            ClientDriveSetInformationResponse::new(&req_inner, NtStatus::NO_SUCH_FILE)
                                .map_err(|e| encode_err!(e))?,
                        );
                        return Ok(vec![SvcMessage::from(res)]);
                    }
                }
                _ => {
                    // TODO
                }
            }
        }
        None => {
            warn!("no such file");
            let res = RdpdrPdu::ClientDriveSetInformationResponse(
                ClientDriveSetInformationResponse::new(&req_inner, NtStatus::NO_SUCH_FILE)
                    .map_err(|e| encode_err!(e))?,
            );
            return Ok(vec![SvcMessage::from(res)]);
        }
    }
    Ok(vec![SvcMessage::from(RdpdrPdu::ClientDriveSetInformationResponse(
        ClientDriveSetInformationResponse::new(&req_inner, NtStatus::SUCCESS)
            .map_err(|e| encode_err!(e))?,
    ))])
}

fn get_file_attributes(meta: &std::fs::Metadata, file_name: &str) -> FileAttributes {
    let mut file_attribute = FileAttributes::empty();
    if meta.is_dir() {
        file_attribute |= FileAttributes::FILE_ATTRIBUTE_DIRECTORY;
    }
    if file_attribute.is_empty() {
        file_attribute |= FileAttributes::FILE_ATTRIBUTE_ARCHIVE;
    }
    if file_name.len() > 1 && file_name.starts_with('.') && file_name.as_bytes()[1] != b'.' {
        file_attribute |= FileAttributes::FILE_ATTRIBUTE_HIDDEN;
    }
    if meta.permissions().readonly() {
        file_attribute |= FileAttributes::FILE_ATTRIBUTE_READONLY;
    }
    file_attribute
}

fn make_query_dir_resp(
    find_file_name: Option<String>,
    device_io_request: DeviceIoRequest,
    file_class: FileInformationClassLevel,
    initial_query: bool,
) -> PduResult<Vec<SvcMessage>> {
    let not_found_status = if initial_query {
        NtStatus::NO_SUCH_FILE
    } else {
        NtStatus::NO_MORE_FILES
    };
    match find_file_name {
        None => Ok(vec![SvcMessage::from(
            RdpdrPdu::ClientDriveQueryDirectoryResponse(ClientDriveQueryDirectoryResponse {
                device_io_reply: DeviceIoResponse::new(device_io_request, not_found_status),
                buffer: None,
            }),
        )]),
        Some(file_full_path) => {
            #[expect(clippy::arithmetic_side_effects)]
            let file_last_sep = file_full_path
                .rfind('/')
                .or_else(|| file_full_path.rfind('\\'))
                .map(|i| i + 1)
                .unwrap_or(0);
            let file_name = &file_full_path[file_last_sep..];
            match std::fs::metadata(&file_full_path) {
                Ok(meta) => {
                    let file_attribute = get_file_attributes(&meta, file_name);
                    if file_class == FileInformationClassLevel::FILE_BOTH_DIRECTORY_INFORMATION {
                        let info = FileBothDirectoryInformation::new(
                            i64::try_from(meta.creation_time()).unwrap_or(0),
                            i64::try_from(meta.creation_time()).unwrap_or(0),
                            i64::try_from(meta.last_access_time()).unwrap_or(0),
                            i64::try_from(meta.last_write_time()).unwrap_or(0),
                            i64::try_from(meta.len()).unwrap_or(0),
                            file_attribute,
                            file_name.to_owned(),
                        );
                        Ok(vec![SvcMessage::from(
                            RdpdrPdu::ClientDriveQueryDirectoryResponse(
                                ClientDriveQueryDirectoryResponse {
                                    device_io_reply: DeviceIoResponse::new(
                                        device_io_request,
                                        NtStatus::SUCCESS,
                                    ),
                                    buffer: Some(FileInformationClass::BothDirectory(info)),
                                },
                            ),
                        )])
                    } else {
                        warn!("unsupported file class for query directory");
                        Ok(vec![SvcMessage::from(
                            RdpdrPdu::ClientDriveQueryDirectoryResponse(
                                ClientDriveQueryDirectoryResponse {
                                    device_io_reply: DeviceIoResponse::new(
                                        device_io_request,
                                        NtStatus::NOT_SUPPORTED,
                                    ),
                                    buffer: None,
                                },
                            ),
                        )])
                    }
                }
                Err(error) => {
                    warn!(%error, "Get metadata error");
                    Ok(vec![SvcMessage::from(
                        RdpdrPdu::ClientDriveQueryDirectoryResponse(
                            ClientDriveQueryDirectoryResponse {
                                device_io_reply: DeviceIoResponse::new(
                                    device_io_request,
                                    not_found_status,
                                ),
                                buffer: None,
                            },
                        ),
                    )])
                }
            }
        }
    }
}

fn query_directory(
    backend: &mut WinRdpdrBackend,
    req_inner: ServerDriveQueryDirectoryRequest,
) -> PduResult<Vec<SvcMessage>> {
    match backend.file_path_map.get(&req_inner.device_io_request.file_id) {
        Some(parent_pos_for_next) => {
            let mut find_file_name = None;

            if req_inner.initial_query > 0 {
                if req_inner.path.ends_with('*') {
                    let mut parent = backend.file_base.clone();
                    let query_path = req_inner.path.replace('\\', "/");
                    #[expect(clippy::arithmetic_side_effects)]
                    let parent_part = &query_path[..query_path.len() - 1];
                    parent.push_str(parent_part);

                    match std::fs::read_dir(&parent) {
                        Ok(mut dir_iter) => {
                            // Find first non-. / .. entry
                            while let Some(Ok(entry)) = dir_iter.next() {
                                let name = entry.file_name();
                                let name_str = name.to_string_lossy();
                                if name_str == "." || name_str == ".." {
                                    continue;
                                }
                                let mut full = parent.clone();
                                if !full.ends_with('/') {
                                    full.push('/');
                                }
                                full.push_str(&name_str);
                                find_file_name = Some(full);
                                break;
                            }
                            backend
                                .file_dir_map
                                .insert(req_inner.device_io_request.file_id, dir_iter);
                        }
                        Err(error) => {
                            warn!(%error, "Failed to read directory: {}", parent);
                        }
                    }
                } else {
                    let mut full_path = backend.file_base.clone();
                    full_path.push_str(&req_inner.path.replace('\\', "/"));
                    find_file_name = Some(full_path);
                }

                make_query_dir_resp(
                    find_file_name,
                    req_inner.device_io_request,
                    req_inner.file_info_class_lvl,
                    true,
                )
            } else {
                if let Some(dir_iter) = backend.file_dir_map.get_mut(&req_inner.device_io_request.file_id) {
                    while let Some(Ok(entry)) = dir_iter.next() {
                        let name = entry.file_name();
                        let name_str = name.to_string_lossy();
                        if name_str == "." || name_str == ".." {
                            continue;
                        }
                        let mut full = parent_pos_for_next.clone();
                        if !full.ends_with('/') {
                            full.push('/');
                        }
                        full.push_str(&name_str);
                        find_file_name = Some(full);
                        break;
                    }
                }

                make_query_dir_resp(
                    find_file_name,
                    req_inner.device_io_request,
                    req_inner.file_info_class_lvl,
                    false,
                )
            }
        }
        None => {
            warn!("no file to query directory");
            Ok(vec![SvcMessage::from(
                RdpdrPdu::ClientDriveQueryDirectoryResponse(ClientDriveQueryDirectoryResponse {
                    device_io_reply: DeviceIoResponse::new(
                        req_inner.device_io_request,
                        NtStatus::NO_SUCH_FILE,
                    ),
                    buffer: None,
                }),
            )])
        }
    }
}

#[expect(clippy::arithmetic_side_effects)]
fn make_create_drive_resp(
    device_io_request: DeviceIoRequest,
    create_disposition: CreateDisposition,
    file_id: u32,
) -> PduResult<Vec<SvcMessage>> {
    let io_response = DeviceIoResponse::new(device_io_request, NtStatus::SUCCESS);
    let information = match create_disposition {
        CreateDisposition::FILE_CREATE
        | CreateDisposition::FILE_SUPERSEDE
        | CreateDisposition::FILE_OPEN
        | CreateDisposition::FILE_OVERWRITE => Information::FILE_SUPERSEDED,
        CreateDisposition::FILE_OPEN_IF => Information::FILE_OPENED,
        CreateDisposition::FILE_OVERWRITE_IF => Information::FILE_OVERWRITTEN,
        _ => Information::empty(),
    };
    Ok(vec![SvcMessage::from(RdpdrPdu::DeviceCreateResponse(
        DeviceCreateResponse {
            device_io_reply: io_response,
            file_id,
            information,
        },
    ))])
}

#[expect(clippy::arithmetic_side_effects)]
fn create_drive(backend: &mut WinRdpdrBackend, req_inner: DeviceCreateRequest) -> PduResult<Vec<SvcMessage>> {
    let file_id = backend.file_id;
    backend.file_id += 1;
    let mut path = backend.file_base.clone();
    path.push_str(&req_inner.path.replace('\\', "/"));

    match std::fs::metadata(&path) {
        Ok(meta) => {
            if meta.is_dir() {
                if req_inner.create_disposition == CreateDisposition::FILE_CREATE {
                    warn!("Attempt to create directory, but it exists");
                    let io_response =
                        DeviceIoResponse::new(req_inner.device_io_request, NtStatus::UNSUCCESSFUL);
                    return Ok(vec![SvcMessage::from(RdpdrPdu::DeviceCreateResponse(
                        DeviceCreateResponse {
                            device_io_reply: io_response,
                            file_id,
                            information: Information::empty(),
                        },
                    ))]);
                }
                if req_inner.create_options.bits() & CreateOptions::FILE_NON_DIRECTORY_FILE.bits() != 0 {
                    warn!("Attempt to create a file, but it is a directory");
                    let io_response =
                        DeviceIoResponse::new(req_inner.device_io_request, NtStatus::UNSUCCESSFUL);
                    return Ok(vec![SvcMessage::from(RdpdrPdu::DeviceCreateResponse(
                        DeviceCreateResponse {
                            device_io_reply: io_response,
                            file_id,
                            information: Information::empty(),
                        },
                    ))]);
                }
            } else if req_inner.create_options.bits() & CreateOptions::FILE_DIRECTORY_FILE.bits() != 0 {
                warn!("Attempt to create a directory, but it is a file");
                let io_response =
                    DeviceIoResponse::new(req_inner.device_io_request, NtStatus::NOT_A_DIRECTORY);
                return Ok(vec![SvcMessage::from(RdpdrPdu::DeviceCreateResponse(
                    DeviceCreateResponse {
                        device_io_reply: io_response,
                        file_id,
                        information: Information::empty(),
                    },
                ))]);
            }
        }
        Err(_) => {
            if req_inner.create_options.bits() & CreateOptions::FILE_DIRECTORY_FILE.bits() != 0 {
                if (req_inner.create_disposition == CreateDisposition::FILE_CREATE
                    || req_inner.create_disposition == CreateDisposition::FILE_OPEN_IF)
                    && std::fs::create_dir_all(&path).is_ok()
                {
                    match std::fs::OpenOptions::new().read(true).open(&path) {
                        Ok(file) => {
                            debug!("create drive dir file_id:{},path:{}", file_id, path);
                            backend.file_map.insert(file_id, file);
                            backend.file_path_map.insert(file_id, path.clone());
                            return make_create_drive_resp(
                                req_inner.device_io_request,
                                req_inner.create_disposition,
                                file_id,
                            );
                        }
                        Err(error) => {
                            warn!(%error, "Open dir error");
                        }
                    }
                }
                let io_response =
                    DeviceIoResponse::new(req_inner.device_io_request, NtStatus::UNSUCCESSFUL);
                return Ok(vec![SvcMessage::from(RdpdrPdu::DeviceCreateResponse(
                    DeviceCreateResponse {
                        device_io_reply: io_response,
                        file_id,
                        information: Information::empty(),
                    },
                ))]);
            }
        }
    }

    let mut fs = std::fs::OpenOptions::new();
    if CreateDisposition::FILE_OPEN_IF == req_inner.create_disposition {
        fs.create(true).write(true).read(true);
    }
    if CreateDisposition::FILE_CREATE == req_inner.create_disposition {
        fs.create_new(true).write(true).read(true);
    }
    if CreateDisposition::FILE_SUPERSEDE == req_inner.create_disposition {
        fs.create(true).write(true).append(true).read(true);
    }
    if CreateDisposition::FILE_OPEN == req_inner.create_disposition {
        fs.read(true);
    }
    if CreateDisposition::FILE_OVERWRITE == req_inner.create_disposition {
        fs.write(true).truncate(true).read(true);
    }
    if CreateDisposition::FILE_OVERWRITE_IF == req_inner.create_disposition {
        fs.write(true).truncate(true).create(true).read(true);
    }

    match fs.open(&path) {
        Ok(file) => {
            debug!("create drive file_id:{},path:{}", file_id, path);
            backend.file_map.insert(file_id, file);
            backend.file_path_map.insert(file_id, path.clone());
            make_create_drive_resp(req_inner.device_io_request, req_inner.create_disposition, file_id)
        }
        Err(error) => {
            warn!(?error, "Open file error for path:{}", path);
            let io_response = DeviceIoResponse::new(req_inner.device_io_request, NtStatus::UNSUCCESSFUL);
            Ok(vec![SvcMessage::from(RdpdrPdu::DeviceCreateResponse(
                DeviceCreateResponse {
                    device_io_reply: io_response,
                    file_id,
                    information: Information::empty(),
                },
            ))])
        }
    }
}

fn process_dependent_file(
    backend: &mut WinRdpdrBackend,
    request: DeviceIoRequest,
    error_fx: impl Fn(DeviceIoRequest) -> PduResult<Vec<SvcMessage>>,
    fx: impl Fn(&mut std::fs::File, DeviceIoRequest) -> PduResult<Vec<SvcMessage>>,
) -> PduResult<Vec<SvcMessage>> {
    match backend.file_map.get_mut(&request.file_id) {
        None => error_fx(request),
        Some(file) => fx(file, request),
    }
}
