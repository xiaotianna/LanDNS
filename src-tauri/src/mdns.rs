#[cfg(target_os = "macos")]
pub use native::register;

#[cfg(not(target_os = "macos"))]
pub async fn register(
    hostnames: &[String],
    _network: &crate::model::NetworkInfo,
) -> Result<Option<tokio::task::JoinHandle<Result<(), String>>>, String> {
    if hostnames.is_empty() {
        Ok(None)
    } else {
        Err("Bonjour 域名注册目前仅支持 macOS。".into())
    }
}

#[cfg(target_os = "macos")]
mod native {
    use crate::model::NetworkInfo;
    use std::{
        collections::BTreeSet,
        ffi::{c_char, c_int, c_void, CString},
        net::Ipv4Addr,
        os::fd::{AsRawFd, RawFd},
        ptr::NonNull,
        time::Duration,
    };
    use tokio::{
        io::{unix::AsyncFd, Interest},
        task::JoinHandle,
        time::timeout,
    };

    type ServiceRef = *mut c_void;
    type RecordRef = *mut c_void;
    type RegisterCallback = unsafe extern "C" fn(ServiceRef, RecordRef, u32, i32, *mut c_void);

    #[link(name = "System")]
    extern "C" {
        fn DNSServiceCreateConnection(reference: *mut ServiceRef) -> i32;
        fn DNSServiceRegisterRecord(
            reference: ServiceRef,
            record: *mut RecordRef,
            flags: u32,
            interface: u32,
            name: *const c_char,
            record_type: u16,
            class: u16,
            length: u16,
            data: *const c_void,
            ttl: u32,
            callback: RegisterCallback,
            context: *mut c_void,
        ) -> i32;
        fn DNSServiceRefSockFD(reference: ServiceRef) -> c_int;
        fn DNSServiceProcessResult(reference: ServiceRef) -> i32;
        fn DNSServiceRefDeallocate(reference: ServiceRef);
    }

    #[repr(C)]
    struct PollFd {
        fd: c_int,
        events: i16,
        revents: i16,
    }

    extern "C" {
        fn if_nametoindex(name: *const c_char) -> u32;
        fn poll(fds: *mut PollFd, count: u32, timeout: c_int) -> c_int;
    }

    struct Registration {
        hostname: String,
        ready: bool,
        error: Option<i32>,
    }

    struct Connection {
        reference: NonNull<c_void>,
        fd: RawFd,
        registrations: Vec<Box<Registration>>,
    }

    // One task owns the connection; callbacks run synchronously inside ProcessResult.
    unsafe impl Send for Connection {}

    impl AsRawFd for Connection {
        fn as_raw_fd(&self) -> RawFd {
            self.fd
        }
    }

    impl Drop for Connection {
        fn drop(&mut self) {
            // AsyncFd deregisters first; this closes the fd and withdraws all owned records.
            unsafe { DNSServiceRefDeallocate(self.reference.as_ptr()) };
        }
    }

    unsafe extern "C" fn registered(
        _reference: ServiceRef,
        _record: RecordRef,
        _flags: u32,
        error: i32,
        context: *mut c_void,
    ) {
        let registration = unsafe { &mut *context.cast::<Registration>() };
        if error == 0 {
            registration.ready = true;
        } else {
            registration.error = Some(error);
        }
    }

    fn error_message(operation: &str, error: i32) -> String {
        let reason = match error {
            -65548 => "名称已被其他服务占用，请更换域名",
            -65563 => "系统 Bonjour 服务未运行",
            -65570 => "系统拒绝了本地网络访问",
            _ => "系统 Bonjour 操作失败",
        };
        format!("{operation}：{reason}（{error}）。")
    }

    async fn next_result(connection: &mut AsyncFd<Connection>) -> Result<bool, String> {
        let mut ready = connection
            .readable_mut()
            .await
            .map_err(|error| format!("无法读取 Bonjour 注册结果：{error}"))?;
        ready.clear_ready();
        let connection = ready.get_inner_mut();
        let mut fd = PollFd {
            fd: connection.fd,
            events: 0x0001,
            revents: 0,
        };
        // ProcessResult blocks on an empty socket, so exclude spurious readiness first.
        let available = unsafe { poll(&mut fd, 1, 0) };
        if available < 0 {
            return Err(format!(
                "Bonjour 连接检查失败：{}",
                std::io::Error::last_os_error()
            ));
        }
        if fd.revents & (0x0008 | 0x0010 | 0x0020) != 0 {
            return Err("系统 Bonjour 连接已中断。".into());
        }
        if available == 0 || fd.revents & 0x0001 == 0 {
            return Ok(false);
        }
        let result = unsafe { DNSServiceProcessResult(connection.reference.as_ptr()) };
        if result != 0 {
            return Err(error_message("Bonjour 注册连接", result));
        }
        for registration in &connection.registrations {
            if let Some(error) = registration.error {
                return Err(error_message(&registration.hostname, error));
            }
        }
        Ok(connection.registrations.iter().all(|entry| entry.ready))
    }

    pub async fn register(
        hostnames: &[String],
        network: &NetworkInfo,
    ) -> Result<Option<JoinHandle<Result<(), String>>>, String> {
        if hostnames.is_empty() {
            return Ok(None);
        }
        let address: Ipv4Addr = network.ip.parse().map_err(|_| "Bonjour IPv4 地址无效")?;
        let interface_name = CString::new(network.interface_name.as_str())
            .map_err(|_| "Bonjour 网络接口名称无效")?;
        let interface = unsafe { if_nametoindex(interface_name.as_ptr()) };
        if interface == 0 {
            return Err(format!(
                "Bonjour 网络接口 {} 不存在。",
                network.interface_name
            ));
        }
        let mut reference = std::ptr::null_mut();
        let result = unsafe { DNSServiceCreateConnection(&mut reference) };
        if result != 0 {
            return Err(error_message("无法连接 Bonjour", result));
        }
        let reference = NonNull::new(reference).ok_or("Bonjour 返回了无效连接")?;
        let mut connection = Connection {
            reference,
            fd: unsafe { DNSServiceRefSockFD(reference.as_ptr()) },
            registrations: Vec::new(),
        };
        if connection.fd < 0 {
            return Err("Bonjour 返回了无效套接字。".into());
        }
        for hostname in hostnames.iter().collect::<BTreeSet<_>>() {
            if !crate::model::valid_dns_name(hostname) || !hostname.ends_with(".local") {
                return Err("Bonjour 注册仅支持有效的 .local 主机名。".into());
            }
            let name = CString::new(format!("{hostname}.")).map_err(|_| "Bonjour 域名无效")?;
            let mut registration = Box::new(Registration {
                hostname: hostname.clone(),
                ready: false,
                error: None,
            });
            let mut record = std::ptr::null_mut();
            let result = unsafe {
                DNSServiceRegisterRecord(
                    reference.as_ptr(),
                    &mut record,
                    0x20,
                    interface,
                    name.as_ptr(),
                    1,
                    1,
                    4,
                    address.octets().as_ptr().cast(),
                    0,
                    registered,
                    (&mut *registration as *mut Registration).cast(),
                )
            };
            if result != 0 {
                return Err(error_message(hostname, result));
            }
            // Box keeps callback context stable when this vector grows or moves into the task.
            connection.registrations.push(registration);
        }
        // The connection owns a stable fd and closes it only after AsyncFd deregistration.
        let mut connection =
            unsafe { AsyncFd::register_with_interest(connection, Interest::READABLE) }
                .map_err(|error| format!("无法监听 Bonjour 注册结果：{error}"))?;
        timeout(Duration::from_secs(5), async {
            while !next_result(&mut connection).await? {}
            Ok::<(), String>(())
        })
        .await
        .map_err(|_| "Bonjour 域名注册超时，请检查局域网连接或名称冲突。".to_owned())??;
        let task: JoinHandle<Result<(), String>> = tokio::spawn(async move {
            loop {
                next_result(&mut connection).await?;
            }
        });
        Ok(Some(task))
    }
}
