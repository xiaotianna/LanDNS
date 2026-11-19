use crate::model::{
    now, Config, LogEntry, NetworkInfo, ServiceStatus, LOG_PATH, SERVICE_CONFIG, STATUS_PATH,
};
use bytes::Bytes;
use hickory_proto::{
    op::{Message, MessageType, OpCode, ResponseCode},
    rr::{rdata::A, DNSClass, RData, Record, RecordType},
};
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full};
use hyper::{
    body::Incoming, header, service::service_fn, HeaderMap, Request, Response, StatusCode,
};
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::{TokioExecutor, TokioIo, TokioTimer},
};
use std::{
    convert::Infallible,
    fs::{self, OpenOptions},
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{watch, Semaphore},
    task::{JoinHandle, JoinSet},
    time::timeout,
};

const DNS_TIMEOUT: Duration = Duration::from_secs(4);
const MAX_QUERY: usize = 4096;
type Body = UnsyncBoxBody<Bytes, hyper::Error>;
type HttpClient = Client<HttpConnector, Incoming>;

#[derive(Default)]
struct Counters {
    dns: AtomicU64,
    proxy: AtomicU64,
}

struct Context {
    config: Config,
    ip: Ipv4Addr,
    mask: Ipv4Addr,
    upstream: SocketAddr,
    counters: Arc<Counters>,
    dns_slots: Arc<Semaphore>,
    http_slots: Arc<Semaphore>,
    client: HttpClient,
    stopped: watch::Receiver<()>,
}

pub fn run() -> Result<(), String> {
    let config: Config =
        serde_json::from_slice(&fs::read(SERVICE_CONFIG).map_err(|e| e.to_string())?)
            .map_err(|e| format!("配置无法读取：{e}"))?;
    config.validate()?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(serve(config))
}

async fn serve(config: Config) -> Result<(), String> {
    let counters = Arc::new(Counters::default());
    let mut status = ServiceStatus {
        running: false,
        version: Some(env!("CARGO_PKG_VERSION").into()),
        pid: Some(std::process::id()),
        ip: None,
        interface_name: None,
        started_at: Some(now()),
        updated_at: Some(now()),
        dns_queries: 0,
        proxy_requests: 0,
        last_error: None,
    };
    let mut tasks: Vec<JoinHandle<Result<(), String>>> = Vec::new();
    let mut stop: Option<watch::Sender<()>> = None;
    let mut active = String::new();
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| e.to_string())?;
    log("info", "后台服务已启动，正在查找局域网接口");
    loop {
        tokio::select! {
            _ = tick.tick() => {},
            _ = terminate.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
        }
        let network = crate::platform::detect_network(&config);
        let next = network
            .as_ref()
            .map(|n| format!("{}:{}:{}", n.interface_name, n.ip, n.netmask));
        let failed = tasks.iter().any(JoinHandle::is_finished);
        if next.as_ref().is_ok_and(|n| n != &active) || next.is_err() || failed || tasks.is_empty()
        {
            drop(stop.take());
            for task in tasks.drain(..) {
                task.abort();
                if let Ok(Err(error)) = task.await {
                    log("error", &error);
                }
            }
            status.running = false;
            active.clear();
            let outcome = match network {
                Ok(network) => {
                    status.ip = Some(network.ip.clone());
                    status.interface_name = Some(network.interface_name.clone());
                    let (stop_sender, stop_receiver) = watch::channel(());
                    match start_listeners(&config, &network, counters.clone(), stop_receiver).await
                    {
                        Ok(new_tasks) => {
                            active = format!(
                                "{}:{}:{}",
                                network.interface_name, network.ip, network.netmask
                            );
                            tasks = new_tasks;
                            stop = Some(stop_sender);
                            status.running = true;
                            log(
                                "info",
                                &format!(
                                    "DNS 已监听 {}:53，泛域名指向 {}{}",
                                    network.ip,
                                    network.ip,
                                    if config.proxy_enabled {
                                        "；HTTP 代理已监听 80 端口"
                                    } else {
                                        ""
                                    }
                                ),
                            );
                            Ok(())
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(error) => {
                    status.ip = None;
                    status.interface_name = None;
                    Err(error)
                }
            };
            let error = outcome.err();
            if error != status.last_error {
                if let Some(error) = &error {
                    log("error", error);
                }
            }
            status.last_error = error;
        }
        status.updated_at = Some(now());
        status.dns_queries = counters.dns.load(Ordering::Relaxed);
        status.proxy_requests = counters.proxy.load(Ordering::Relaxed);
        write_status(&status)?;
    }
    drop(stop);
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
    status.running = false;
    status.updated_at = Some(now());
    write_status(&status)?;
    log("info", "后台服务已停止");
    Ok(())
}

async fn start_listeners(
    config: &Config,
    network: &NetworkInfo,
    counters: Arc<Counters>,
    stopped: watch::Receiver<()>,
) -> Result<Vec<JoinHandle<Result<(), String>>>, String> {
    let ip: Ipv4Addr = network.ip.parse().map_err(|_| "局域网 IPv4 地址无效")?;
    let mask: Ipv4Addr = network.netmask.parse().map_err(|_| "局域网子网掩码无效")?;
    let upstream_ip: Ipv4Addr = config
        .upstream_dns
        .parse()
        .map_err(|_| "上游 DNS 地址无效")?;
    let local_addresses =
        if_addrs::get_if_addrs().map_err(|e| format!("无法检查本机网络地址：{e}"))?;
    if upstream_ip == ip
        || upstream_ip.is_loopback()
        || local_addresses
            .iter()
            .any(|interface| interface.ip() == IpAddr::V4(upstream_ip))
    {
        return Err("上游 DNS 不能指向本机，避免循环解析".into());
    }
    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(Duration::from_secs(5)));
    let context = Arc::new(Context {
        config: config.clone(),
        ip,
        mask,
        upstream: SocketAddr::new(upstream_ip.into(), 53),
        counters,
        dns_slots: Arc::new(Semaphore::new(128)),
        http_slots: Arc::new(Semaphore::new(128)),
        stopped,
        client: Client::builder(TokioExecutor::new())
            .pool_timer(TokioTimer::new())
            .pool_idle_timeout(Duration::from_secs(30))
            .build(connector),
    });
    // Bind everything before spawning so a busy port cannot leave a half-running service.
    let mut sockets = Vec::new();
    for address in [Ipv4Addr::LOCALHOST, ip] {
        let udp = UdpSocket::bind((address, 53))
            .await
            .map_err(|e| format!("无法监听 {address}:53 UDP（端口可能已被占用）：{e}"))?;
        let tcp = TcpListener::bind((address, 53))
            .await
            .map_err(|e| format!("无法监听 {address}:53 TCP（端口可能已被占用）：{e}"))?;
        let http = if config.proxy_enabled {
            Some(
                TcpListener::bind((address, 80))
                    .await
                    .map_err(|e| format!("无法监听 {address}:80（端口可能已被占用）：{e}"))?,
            )
        } else {
            None
        };
        sockets.push((udp, tcp, http));
    }
    let mut tasks = Vec::new();
    if let Some(task) = crate::mdns::register(&config.bonjour_hostnames()?, network).await? {
        tasks.push(task);
    }
    for (udp, tcp, http) in sockets {
        tasks.push(tokio::spawn(udp_server(udp, context.clone())));
        tasks.push(tokio::spawn(tcp_server(tcp, context.clone(), false)));
        if let Some(http) = http {
            tasks.push(tokio::spawn(tcp_server(http, context.clone(), true)));
        }
    }
    Ok(tasks)
}

fn allowed(peer: IpAddr, local: Ipv4Addr, mask: Ipv4Addr) -> bool {
    match peer {
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || (u32::from(ip) & u32::from(mask)) == (u32::from(local) & u32::from(mask))
        }
        IpAddr::V6(_) => false,
    }
}

async fn udp_server(socket: UdpSocket, context: Arc<Context>) -> Result<(), String> {
    let socket = Arc::new(socket);
    let mut buffer = [0u8; MAX_QUERY + 1];
    let mut requests = JoinSet::new();
    loop {
        tokio::select! {
            received = socket.recv_from(&mut buffer) => {
                let (length, peer) = received.map_err(|e| e.to_string())?;
                if length > MAX_QUERY || !allowed(peer.ip(), context.ip, context.mask) { continue; }
                let Ok(permit) = context.dns_slots.clone().try_acquire_owned() else { continue; };
                let packet = buffer[..length].to_vec();
                let context = context.clone();
                let socket = socket.clone();
                requests.spawn(async move {
                    let _permit = permit;
                    if let Some(reply) = dns_reply(&packet, &context, false).await {
                        let _ = socket.send_to(&reply, peer).await;
                    }
                });
            }
            _ = requests.join_next(), if !requests.is_empty() => {},
        }
    }
}

async fn tcp_server(
    listener: TcpListener,
    context: Arc<Context>,
    http: bool,
) -> Result<(), String> {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = accepted.map_err(|e| e.to_string())?;
                if !allowed(peer.ip(), context.ip, context.mask) { continue; }
                let slots = if http { &context.http_slots } else { &context.dns_slots };
                let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                let context = context.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    if http {
                        let service = service_fn(move |request| proxy(request, context.clone(), peer));
                        let _ = hyper::server::conn::http1::Builder::new()
                            .timer(TokioTimer::new())
                            .header_read_timeout(Duration::from_secs(15))
                            .max_buf_size(32 * 1024)
                            .serve_connection(TokioIo::new(stream), service)
                            .with_upgrades().await;
                    } else {
                        let _ = tcp_dns(stream, context).await;
                    }
                });
            }
            _ = connections.join_next(), if !connections.is_empty() => {},
        }
    }
}

async fn tcp_dns(mut stream: TcpStream, context: Arc<Context>) -> Result<(), std::io::Error> {
    loop {
        let length = timeout(Duration::from_secs(10), stream.read_u16()).await?? as usize;
        if !(12..=MAX_QUERY).contains(&length) {
            return Ok(());
        }
        let mut packet = vec![0; length];
        timeout(Duration::from_secs(10), stream.read_exact(&mut packet)).await??;
        let Some(reply) = dns_reply(&packet, &context, true).await else {
            return Ok(());
        };
        timeout(Duration::from_secs(10), async {
            stream.write_u16(reply.len() as u16).await?;
            stream.write_all(&reply).await
        })
        .await??;
    }
}

fn base_reply(request: &Message, code: ResponseCode) -> Message {
    let mut response = Message::new();
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .set_op_code(request.op_code())
        .set_recursion_desired(request.recursion_desired())
        .set_recursion_available(true)
        .set_response_code(code);
    response.add_queries(request.queries().iter().cloned());
    response
}

fn local_reply(request: &Message, config: &Config, ip: Ipv4Addr) -> Option<Message> {
    if request.op_code() != OpCode::Query {
        return Some(base_reply(request, ResponseCode::NotImp));
    }
    if request.queries().len() != 1 {
        return Some(base_reply(request, ResponseCode::FormErr));
    }
    let query = &request.queries()[0];
    if query.query_class() != DNSClass::IN {
        return Some(base_reply(request, ResponseCode::Refused));
    }
    let name = query
        .name()
        .to_utf8()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if config.resolves_hostname(&name) {
        let mut response = base_reply(request, ResponseCode::NoError);
        response.set_authoritative(true);
        if query.query_type() == RecordType::A {
            response.add_answer(Record::from_rdata(
                query.name().clone(),
                config.ttl,
                RData::A(A(ip)),
            ));
        }
        return Some(response);
    }
    // RFC 8375 requires home.arpa names to stay inside the home network.
    if name == "home.arpa" || name.ends_with(".home.arpa") {
        return Some(base_reply(request, ResponseCode::NXDomain));
    }
    None
}

async fn dns_reply(packet: &[u8], context: &Context, tcp: bool) -> Option<Vec<u8>> {
    let request = Message::from_vec(packet).ok()?;
    if request.message_type() != MessageType::Query {
        return None;
    }
    context.counters.dns.fetch_add(1, Ordering::Relaxed);
    let mut response = if let Some(response) = local_reply(&request, &context.config, context.ip) {
        response
    } else {
        match timeout(
            DNS_TIMEOUT,
            forward_dns(packet, &request, context.upstream, tcp),
        )
        .await
        {
            Ok(Ok(response)) => response,
            _ => base_reply(&request, ResponseCode::ServFail),
        }
    };
    let wire = response.to_vec().ok()?;
    if !tcp && wire.len() > usize::from(request.max_payload().min(4096)) {
        response = base_reply(&request, response.response_code());
        response.set_truncated(true);
        response.to_vec().ok()
    } else {
        Some(wire)
    }
}

async fn forward_dns(
    packet: &[u8],
    request: &Message,
    upstream: SocketAddr,
    tcp: bool,
) -> Result<Message, String> {
    let reply = if tcp {
        let mut stream = TcpStream::connect(upstream)
            .await
            .map_err(|e| e.to_string())?;
        stream
            .write_u16(packet.len() as u16)
            .await
            .map_err(|e| e.to_string())?;
        stream.write_all(packet).await.map_err(|e| e.to_string())?;
        let length = stream.read_u16().await.map_err(|e| e.to_string())? as usize;
        if length < 12 {
            return Err("上游 DNS 响应过短".into());
        }
        let mut bytes = vec![0; length];
        stream
            .read_exact(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        bytes
    } else {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .await
            .map_err(|e| e.to_string())?;
        socket.connect(upstream).await.map_err(|e| e.to_string())?;
        socket.send(packet).await.map_err(|e| e.to_string())?;
        let mut bytes = vec![0; u16::MAX as usize];
        let length = socket.recv(&mut bytes).await.map_err(|e| e.to_string())?;
        bytes.truncate(length);
        bytes
    };
    let response = Message::from_vec(&reply).map_err(|e| e.to_string())?;
    if response.message_type() != MessageType::Response
        || response.id() != request.id()
        || response.op_code() != request.op_code()
        || response.queries() != request.queries()
    {
        return Err("上游 DNS 响应不匹配".into());
    }
    Ok(response)
}

fn route_port(host: &str, config: &Config) -> Option<u16> {
    let name = host.trim_end_matches('.').to_ascii_lowercase();
    config
        .routes
        .iter()
        .find(|r| r.enabled && r.subdomain != "*" && r.effective_hostname(&config.domain) == name)
        .or_else(|| {
            name.strip_suffix(&format!(".{}", config.domain))
                .filter(|subdomain| !subdomain.is_empty())?;
            config
                .routes
                .iter()
                .find(|r| r.enabled && r.subdomain == "*")
        })
        .map(|r| r.port)
}

fn strip_hop_headers(headers: &mut HeaderMap) {
    let named: Vec<String> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|name| name.trim().to_owned())
        .collect();
    for name in named {
        headers.remove(name.as_str());
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

fn text_response(status: StatusCode, text: &'static str) -> Response<Body> {
    let body = Full::new(Bytes::from_static(text.as_bytes()))
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

async fn proxy(
    mut request: Request<Incoming>,
    context: Arc<Context>,
    peer: SocketAddr,
) -> Result<Response<Body>, Infallible> {
    context.counters.proxy.fetch_add(1, Ordering::Relaxed);
    if request.method() == hyper::Method::CONNECT {
        return Ok(text_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "LanDNS 不支持 CONNECT 隧道",
        ));
    }
    let original_host = request.headers().get(header::HOST).cloned();
    let host = original_host
        .as_ref()
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<hyper::http::uri::Authority>().ok())
        .map(|authority| authority.host().to_owned());
    let Some((host, port)) =
        host.and_then(|host| route_port(&host, &context.config).map(|port| (host, port)))
    else {
        return Ok(text_response(
            StatusCode::NOT_FOUND,
            "LanDNS：此域名尚未配置代理规则，请在应用中添加。",
        ));
    };
    if port == 80 || port == 53 || port == 0 {
        return Ok(text_response(
            StatusCode::BAD_GATEWAY,
            "LanDNS：目标端口会造成服务回环",
        ));
    }
    let websocket = request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
        && request
            .headers()
            .get_all(header::CONNECTION)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| {
                v.split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            });
    let on_client_upgrade = websocket.then(|| hyper::upgrade::on(&mut request));
    strip_hop_headers(request.headers_mut());
    if let Some(value) = original_host {
        request.headers_mut().insert(header::HOST, value);
    }
    let untrusted: Vec<_> = request
        .headers()
        .keys()
        .filter(|name| {
            name.as_str() == "forwarded"
                || name.as_str() == "x-real-ip"
                || name.as_str().starts_with("x-forwarded-")
        })
        .cloned()
        .collect();
    for name in untrusted {
        request.headers_mut().remove(name);
    }
    request.headers_mut().insert(
        "x-forwarded-for",
        header::HeaderValue::from_str(&peer.ip().to_string()).unwrap(),
    );
    request.headers_mut().insert(
        "x-forwarded-proto",
        header::HeaderValue::from_static("http"),
    );
    if let Ok(value) = header::HeaderValue::from_str(&host) {
        request.headers_mut().insert("x-forwarded-host", value);
    }
    if websocket {
        request.headers_mut().insert(
            header::CONNECTION,
            header::HeaderValue::from_static("upgrade"),
        );
        request.headers_mut().insert(
            header::UPGRADE,
            header::HeaderValue::from_static("websocket"),
        );
    }
    let path = request
        .uri()
        .path_and_query()
        .map_or("/", |value| value.as_str());
    let Ok(uri) = format!("http://127.0.0.1:{port}{path}").parse() else {
        return Ok(text_response(
            StatusCode::BAD_REQUEST,
            "LanDNS：请求地址无效",
        ));
    };
    *request.uri_mut() = uri;
    let mut response = match timeout(Duration::from_secs(30), context.client.request(request)).await
    {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => {
            return Ok(text_response(
                StatusCode::BAD_GATEWAY,
                "LanDNS：无法连接本机应用，请确认目标端口的服务已启动。",
            ))
        }
        Err(_) => {
            return Ok(text_response(
                StatusCode::GATEWAY_TIMEOUT,
                "LanDNS：本机应用响应超时。",
            ))
        }
    };
    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        let Some(client_upgrade) = on_client_upgrade else {
            return Ok(text_response(
                StatusCode::BAD_GATEWAY,
                "LanDNS：上游返回了意外的协议升级",
            ));
        };
        let Ok(permit) = context.http_slots.clone().try_acquire_owned() else {
            return Ok(text_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "LanDNS：连接数已达上限",
            ));
        };
        let server_upgrade = hyper::upgrade::on(&mut response);
        let mut stopped = context.stopped.clone();
        tokio::spawn(async move {
            let _permit = permit;
            tokio::select! {
                // Dropping the listener generation's sender also closes upgraded sockets.
                _ = stopped.changed() => {},
                _ = async {
                    if let Ok(Ok((client, server))) = timeout(Duration::from_secs(10), async {
                        Ok::<_, hyper::Error>((client_upgrade.await?, server_upgrade.await?))
                    }).await {
                        let _ = tokio::io::copy_bidirectional(
                            &mut TokioIo::new(client), &mut TokioIo::new(server),
                        ).await;
                    }
                } => {},
            }
        });
        strip_hop_headers(response.headers_mut());
        response.headers_mut().insert(
            header::CONNECTION,
            header::HeaderValue::from_static("upgrade"),
        );
        response.headers_mut().insert(
            header::UPGRADE,
            header::HeaderValue::from_static("websocket"),
        );
    } else {
        strip_hop_headers(response.headers_mut());
    }
    Ok(response.map(BodyExt::boxed_unsync))
}

fn write_status(status: &ServiceStatus) -> Result<(), String> {
    let bytes = serde_json::to_vec(status).map_err(|e| e.to_string())?;
    let temporary = format!("{STATUS_PATH}.tmp");
    fs::write(&temporary, bytes)
        .and_then(|_| fs::rename(&temporary, STATUS_PATH))
        .map_err(|e| e.to_string())
}

fn log(level: &str, message: &str) {
    if fs::metadata(LOG_PATH).is_ok_and(|m| m.len() > 512 * 1024) {
        let _ = fs::rename(LOG_PATH, format!("{LOG_PATH}.1"));
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(LOG_PATH) {
        let entry = LogEntry {
            timestamp: now(),
            level: level.into(),
            message: message.into(),
        };
        if let Ok(line) = serde_json::to_string(&entry) {
            let _ = writeln!(file, "{line}");
        }
    }
}

#[cfg(test)]
mod checks {
    use super::*;
    use crate::model::ProxyRoute;
    use hickory_proto::{op::Query, rr::Name};

    #[test]
    fn proxy_prefers_exact_routes_and_removes_connection_scoped_headers() {
        let mut config = Config::default();
        config.routes = vec![
            ProxyRoute {
                id: "fallback".into(),
                name: "Fallback".into(),
                subdomain: "*".into(),
                hostname: None,
                port: 3000,
                enabled: true,
            },
            ProxyRoute {
                id: "api".into(),
                name: "API".into(),
                subdomain: "api".into(),
                hostname: None,
                port: 8080,
                enabled: true,
            },
            ProxyRoute {
                id: "custom".into(),
                name: "Custom".into(),
                subdomain: "app".into(),
                hostname: Some("example.com".into()),
                port: 8081,
                enabled: true,
            },
        ];
        assert_eq!(route_port("API.dev.home.arpa.", &config), Some(8080));
        assert_eq!(route_port("nested.api.dev.home.arpa", &config), Some(3000));
        assert_eq!(route_port("api.notdev.home.arpa", &config), None);
        assert_eq!(route_port("dev.home.arpa", &config), None);
        assert_eq!(route_port("EXAMPLE.COM.", &config), Some(8081));
        for host in ["com", "sub.example.com", "otherexample.com"] {
            assert_eq!(route_port(host, &config), None, "{host}");
        }
        config.routes[2].hostname = Some("custom.dev.home.arpa".into());
        assert_eq!(route_port("custom.dev.home.arpa", &config), Some(8081));
        assert_eq!(
            route_port("child.custom.dev.home.arpa", &config),
            Some(3000)
        );
        config.routes[2].hostname = Some("dev.home.arpa".into());
        assert_eq!(route_port("dev.home.arpa", &config), Some(8081));
        config.routes[2].enabled = false;
        assert_eq!(route_port("dev.home.arpa", &config), None);
        config.routes[1].enabled = false;
        assert_eq!(route_port("api.dev.home.arpa", &config), Some(3000));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            header::HeaderValue::from_static("keep-alive, x-private"),
        );
        headers.insert("x-private", header::HeaderValue::from_static("secret"));
        headers.insert(
            header::HOST,
            header::HeaderValue::from_static("api.dev.home.arpa:80"),
        );
        strip_hop_headers(&mut headers);
        assert!(!headers.contains_key(header::CONNECTION));
        assert!(!headers.contains_key("x-private"));
        assert_eq!(headers[header::HOST], "api.dev.home.arpa:80");
    }

    #[test]
    fn local_dns_preserves_zone_boundaries_and_never_leaks_home_arpa() {
        let mut config = Config::default();
        config.routes.push(ProxyRoute {
            id: "custom".into(),
            name: "Custom".into(),
            subdomain: "app".into(),
            hostname: Some("example.com".into()),
            port: 8080,
            enabled: true,
        });
        let ip = Ipv4Addr::new(192, 168, 1, 42);
        for (name, kind, expected) in [
            ("a.dev.home.arpa.", RecordType::A, Some(1)),
            ("nested.a.dev.home.arpa.", RecordType::A, Some(1)),
            ("dev.home.arpa.", RecordType::AAAA, Some(0)),
            ("notdev.home.arpa.", RecordType::A, Some(0)),
            ("EXAMPLE.COM.", RecordType::A, Some(1)),
            ("example.com.", RecordType::AAAA, Some(0)),
            ("com.", RecordType::A, None),
            ("sub.example.com.", RecordType::A, None),
            ("otherexample.com.", RecordType::A, None),
        ] {
            let mut query = Message::new();
            query.add_query(Query::query(Name::from_ascii(name).unwrap(), kind));
            let response = local_reply(&query, &config, ip);
            assert_eq!(response.map(|r| r.answers().len()), expected);
        }
        config.routes[0].enabled = false;
        let mut query = Message::new();
        query.add_query(Query::query(
            Name::from_ascii("example.com.").unwrap(),
            RecordType::A,
        ));
        assert!(local_reply(&query, &config, ip).is_none());
        assert!(allowed(
            "192.168.1.8".parse().unwrap(),
            ip,
            "255.255.255.0".parse().unwrap()
        ));
        assert!(!allowed(
            "192.168.2.8".parse().unwrap(),
            ip,
            "255.255.255.0".parse().unwrap()
        ));
    }
}
