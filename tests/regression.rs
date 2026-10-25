//! 回归验证（对应 Python 版 verify_fixes.py）：
//! 修复 #1/#2/#3 后的正确性 + 不回归检查。
//! 被测路径全部走 fake 节点（127.0.0.1:44444），不依赖真实百度节点。
//!
//! 运行: cargo test --test regression -- --nocapture

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const HOST: &str = "127.0.0.1";
const PROXY_PORT: u16 = 10002;
const FAKE_PORT: u16 = 44444;
const ECHO_PORT: u16 = 45555;
const HALF_PORT: u16 = 45557;
const HTTP_PORT: u16 = 45556;

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

// ------------------------------ 测试服务端 -----------------------------------

fn spawn_echo_server() {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind((HOST, ECHO_PORT)) {
            Ok(l) => l,
            Err(_) => return,
        };
        for conn in listener.incoming() {
            let mut c = match conn {
                Ok(c) => c,
                Err(_) => continue,
            };
            std::thread::spawn(move || {
                let _ = c.set_nodelay(true);
                let mut buf = [0u8; 65536];
                loop {
                    match c.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if c.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                    }
                }
                let _ = c.shutdown(Shutdown::Both);
            });
        }
    });
}

fn spawn_halfclose_server() {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind((HOST, HALF_PORT)) {
            Ok(l) => l,
            Err(_) => return,
        };
        for conn in listener.incoming() {
            let mut c = match conn {
                Ok(c) => c,
                Err(_) => continue,
            };
            std::thread::spawn(move || {
                let _ = c.set_read_timeout(Some(Duration::from_secs(30)));
                let mut data = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    match c.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => data.extend_from_slice(&buf[..n]),
                    }
                }
                let mut reply = b"eof:".to_vec();
                reply.extend_from_slice(&data);
                let _ = c.write_all(&reply);
                let _ = c.shutdown(Shutdown::Both);
            });
        }
    });
}

fn big_body() -> Vec<u8> {
    vec![b'x'; 4 * 1024 * 1024]
}

fn spawn_http_server(served: Arc<Mutex<Vec<String>>>) {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind((HOST, HTTP_PORT)) {
            Ok(l) => l,
            Err(_) => return,
        };
        let big = big_body();
        for conn in listener.incoming() {
            let mut c = match conn {
                Ok(c) => c,
                Err(_) => continue,
            };
            let served = served.clone();
            let big = big.clone();
            std::thread::spawn(move || {
                // 手写 HTTP/1.1 服务端：支持 keep-alive，收到 connection: close 即断开。
                while let Some(head) = read_http_head(&mut c) {
                    let text = String::from_utf8_lossy(&head).into_owned();
                    let mut lines = text.split("\r\n");
                    let reqline = lines.next().unwrap_or("");
                    let mut rp = reqline.split_whitespace();
                    let _method = rp.next();
                    let path = rp.next().unwrap_or("");
                    let version = rp.next().unwrap_or("HTTP/1.0");
                    let mut host_value = "?".to_string();
                    let mut close_req = false;
                    for line in lines {
                        if line.is_empty() {
                            break;
                        }
                        let lower = line.to_ascii_lowercase();
                        if let Some(rest) = lower.strip_prefix("host:") {
                            let _ = rest;
                            host_value = line["host:".len()..].trim().to_string();
                        }
                        if lower.starts_with("connection:") && lower.contains("close") {
                            close_req = true;
                        }
                    }
                    served.lock().unwrap().push(host_value);
                    let body: &[u8] = if path == "/big" { &big } else { b"target-body" };
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                    if c.write_all(resp.as_bytes()).is_err() || c.write_all(body).is_err() {
                        break;
                    }
                    let keep_alive = version == "HTTP/1.1" && !close_req;
                    if !keep_alive {
                        break;
                    }
                }
                let _ = c.shutdown(Shutdown::Both);
            });
        }
    });
}

fn read_http_head(c: &mut TcpStream) -> Option<Vec<u8>> {
    c.set_read_timeout(Some(Duration::from_secs(15))).ok()?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            buf.truncate(i + 4);
            return Some(buf);
        }
        match c.read(&mut tmp) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
        if buf.len() > 1024 * 1024 {
            return None;
        }
    }
}

// ------------------------------ 工具 ----------------------------------------

struct Children(Child, Child);

impl Drop for Children {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = self.1.kill();
        let _ = self.1.wait();
    }
}

#[derive(Default)]
struct Results {
    items: Vec<(String, bool, String)>,
}

impl Results {
    fn check(&mut self, name: &str, ok: bool, detail: &str) {
        let tag = if ok { "PASS" } else { "FAIL" };
        if detail.is_empty() {
            println!("[{tag}] {name}");
        } else {
            println!("[{tag}] {name}  ({detail})");
        }
        self.items.push((name.into(), ok, detail.into()));
    }
}

fn recvn(s: &mut TcpStream, n: usize, timeout: Duration) -> Vec<u8> {
    let _ = s.set_read_timeout(Some(timeout));
    let mut buf = Vec::new();
    while buf.len() < n {
        let want = n - buf.len();
        let mut tmp = vec![0u8; want];
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(k) => buf.extend_from_slice(&tmp[..k]),
        }
    }
    buf
}

fn read_until_close(s: &mut TcpStream, timeout: Duration) -> Vec<u8> {
    let _ = s.set_read_timeout(Some(timeout));
    let mut out = Vec::new();
    let mut tmp = [0u8; 65536];
    loop {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => out.extend_from_slice(&tmp[..n]),
        }
    }
    out
}

/// 建 SOCKS5 连接并返回 (socket, greeting_reply, connect_reply)。
fn socks_connect(
    port: u16,
    methods: &[u8],
    pack_payload: Option<&[u8]>,
) -> (TcpStream, Vec<u8>, Vec<u8>) {
    let mut s = TcpStream::connect((HOST, PROXY_PORT)).expect("connect to proxy");
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(15))).unwrap();
    let mut greeting = vec![0x05u8, methods.len() as u8];
    greeting.extend_from_slice(methods);
    s.write_all(&greeting).unwrap();
    let greet = recvn(&mut s, 2, Duration::from_secs(15));
    if greet != vec![0x05, 0x00] {
        return (s, greet, Vec::new());
    }
    let mut req = vec![0x05u8, 0x01, 0x00, 0x01, 127, 0, 0, 1];
    req.extend_from_slice(&port.to_be_bytes());
    match pack_payload {
        Some(p) => {
            // 握手与业务数据合并成一个包
            req.extend_from_slice(p);
            s.write_all(&req).unwrap();
        }
        None => s.write_all(&req).unwrap(),
    }
    let rep = recvn(&mut s, 10, Duration::from_secs(15));
    (s, greet, rep)
}

fn wait_listening(ports: &[u16], budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    'outer: loop {
        for p in ports {
            match TcpStream::connect((HOST, *p)) {
                Ok(_) => {}
                Err(_) => {
                    if Instant::now() >= deadline {
                        return false;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    continue 'outer;
                }
            }
        }
        return true;
    }
}

// ------------------------------ 用例 ----------------------------------------

fn case1_connect_early_data(r: &mut Results) {
    // #1 CONNECT 头后同包携带的应用数据必须送达目标。
    let mut payload = b"EARLY-DATA-".to_vec();
    payload.extend(32u8..64);
    let mut s = TcpStream::connect((HOST, PROXY_PORT)).expect("connect proxy");
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(15))).unwrap();
    let mut req = format!("CONNECT {HOST}:{ECHO_PORT} HTTP/1.1\r\nHost: x\r\n\r\n").into_bytes();
    req.extend_from_slice(&payload);
    s.write_all(&req).unwrap();
    let head = recvn(&mut s, 39, Duration::from_secs(15));
    if !head.starts_with(b"HTTP/1.1 200") {
        r.check(
            "#1 CONNECT 早到数据",
            false,
            &format!("握手失败 {:?}", &head[..head.len().min(40)]),
        );
        return;
    }
    let got = recvn(&mut s, payload.len(), Duration::from_secs(15));
    r.check(
        "#1 CONNECT 早到数据（同包 payload 送达）",
        got == payload,
        &format!("got={:?}...", &got[..got.len().min(20)]),
    );
}

fn case1b_socks_early_data(r: &mut Results) {
    // #1 对照组：SOCKS5 早到数据（应本来就正常，确认未回归）。
    let payload = b"SOCKS-EARLY-DATA".to_vec();
    let (mut s, _greet, rep) = socks_connect(ECHO_PORT, &[0x00], Some(&payload));
    if rep.len() < 2 || rep[1] != 0 {
        r.check(
            "#1b SOCKS5 早到数据",
            false,
            &format!("connect reply={:02x?}", rep),
        );
        return;
    }
    let got = recvn(&mut s, payload.len(), Duration::from_secs(15));
    r.check(
        "#1b SOCKS5 早到数据（未回归）",
        got == payload,
        &format!("got={got:?}"),
    );
}

fn case1c_large_early_data(r: &mut Results) {
    // #1 早到数据量超过单次 read 缓冲（>8192）时不能截断。
    let base: Vec<u8> = (0u8..=255).collect();
    let mut payload = Vec::with_capacity(51200);
    for _ in 0..200 {
        payload.extend_from_slice(&base);
    }
    let mut s = TcpStream::connect((HOST, PROXY_PORT)).expect("connect proxy");
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut req = format!("CONNECT {HOST}:{ECHO_PORT} HTTP/1.1\r\nHost: x\r\n\r\n").into_bytes();
    req.extend_from_slice(&payload);
    s.write_all(&req).unwrap();
    let mut head = Vec::new();
    let mut one = [0u8; 1];
    while !find(&head, b"\r\n\r\n").is_some() {
        match s.read(&mut one) {
            Ok(0) | Err(_) => break,
            Ok(_) => head.push(one[0]),
        }
    }
    let got = recvn(&mut s, payload.len(), Duration::from_secs(30));
    r.check(
        "#1c CONNECT 大块早到数据（50KB 不截断）",
        got == payload,
        &format!("got {}/{} bytes", got.len(), payload.len()),
    );
}

fn case7_connect_pipelined_tls_like(r: &mut Results) {
    // #1 真实形态：CONNECT + 类 TLS ClientHello 首包，必须完整透传。
    let mut hello = vec![0x16u8, 0x03, 0x01, 0x02, 0x00, 0x01];
    let pat: Vec<u8> = (0u8..=255).collect();
    hello.extend(pat.repeat(2));
    let mut s = TcpStream::connect((HOST, PROXY_PORT)).expect("connect proxy");
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut req = format!(
        "CONNECT {HOST}:{ECHO_PORT} HTTP/1.1\r\nHost: example.com:443\r\nProxy-Connection: Keep-Alive\r\n\r\n"
    )
    .into_bytes();
    req.extend_from_slice(&hello);
    s.write_all(&req).unwrap();
    let mut head = Vec::new();
    let mut one = [0u8; 1];
    while !find(&head, b"\r\n\r\n").is_some() {
        match s.read(&mut one) {
            Ok(0) | Err(_) => break,
            Ok(_) => head.push(one[0]),
        }
    }
    let got = recvn(&mut s, hello.len(), Duration::from_secs(20));
    r.check(
        "#7 CONNECT 后 pipeline 首包（类 TLS）完整",
        got == hello,
        &format!("{}/{} bytes", got.len(), hello.len()),
    );
}

fn case2_no_crosstalk(r: &mut Results, served: &Arc<Mutex<Vec<String>>>) {
    // #2 明文 HTTP 入站连接不应把第二个目标塞进第一个隧道。
    served.lock().unwrap().clear();
    let mut s = TcpStream::connect((HOST, PROXY_PORT)).expect("connect proxy");
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(15))).unwrap();
    let req = format!(
        "GET http://{HOST}:{HTTP_PORT}/ HTTP/1.1\r\nHost: {HOST}:{HTTP_PORT}\r\nConnection: keep-alive\r\n\r\n"
    );
    s.write_all(req.as_bytes()).unwrap();
    let first = read_until_close(&mut s, Duration::from_secs(20));
    // 代理应主动关闭（Connection: close），且只投递一个请求
    let body_ok = find(&first, b"target-body").is_some();
    let served_snapshot = served.lock().unwrap().clone();
    let only_one = served_snapshot.len() == 1;
    r.check(
        "#2 明文 HTTP 单请求、不串台",
        body_ok && only_one,
        &format!("served={served_snapshot:?} body_ok={body_ok}"),
    );
}

fn case2b_absolute_uri_still_works(r: &mut Results, served: &Arc<Mutex<Vec<String>>>) {
    // #2 不回归：普通绝对 URI 请求仍正常。
    served.lock().unwrap().clear();
    let mut s = TcpStream::connect((HOST, PROXY_PORT)).expect("connect proxy");
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(20))).unwrap();
    let req = format!(
        "GET http://{HOST}:{HTTP_PORT}/ HTTP/1.1\r\nHost: {HOST}\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).unwrap();
    let data = read_until_close(&mut s, Duration::from_secs(20));
    let served_snapshot = served.lock().unwrap().clone();
    r.check(
        "#2b 绝对 URI 请求未回归",
        find(&data, b"target-body").is_some() && served_snapshot == vec!["127.0.0.1".to_string()],
        &format!("served={served_snapshot:?}"),
    );
}

fn case2c_big_body(r: &mut Results) {
    // #2 不回归：大响应体完整性。
    let mut s = TcpStream::connect((HOST, PROXY_PORT)).expect("connect proxy");
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(60))).unwrap();
    let req = format!(
        "GET http://{HOST}:{HTTP_PORT}/big HTTP/1.1\r\nHost: {HOST}\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).unwrap();
    let data = read_until_close(&mut s, Duration::from_secs(60));
    let body_start = find(&data, b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(data.len());
    let body = &data[body_start..];
    let big = big_body();
    let ok = body.len() == big.len() && body.iter().all(|&b| b == b'x');
    r.check("#2c 4MB 响应完整性", ok, &format!("{} bytes", body.len()));
}

fn case3_method_negotiation(r: &mut Results) {
    // #3 客户端未提供 0x00 时应回 0xFF 并断开。
    let mut s = TcpStream::connect((HOST, PROXY_PORT)).expect("connect proxy");
    s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(8))).unwrap();
    s.write_all(&[0x05, 0x01, 0x02]).unwrap(); // 只支持 user/pass
    let rep = recvn(&mut s, 2, Duration::from_secs(8));
    let extra = recvn(&mut s, 10, Duration::from_secs(3)); // 不应再收到 CONNECT 应答
    r.check(
        "#3 无合适方法时回 0xFF",
        rep == vec![0x05, 0xff] && extra.is_empty(),
        &format!("reply={rep:02x?} extra={extra:02x?}"),
    );
}

fn case3b_noauth_ok(r: &mut Results) {
    // #3 不回归：声明 0x00 时正常协商。
    let mut s = TcpStream::connect((HOST, PROXY_PORT)).expect("connect proxy");
    s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(8))).unwrap();
    s.write_all(&[0x05, 0x03, 0x00, 0x01, 0x02]).unwrap(); // 提供 0x00 也提供别的
    let rep = recvn(&mut s, 2, Duration::from_secs(8));
    r.check(
        "#3b 提供 0x00 时正常协商",
        rep == vec![0x05, 0x00],
        &format!("reply={rep:02x?}"),
    );
}

fn case3c_multi_methods_no_noauth(r: &mut Results) {
    // #3 提供多个方法但都不含 0x00 -> 仍须 0xFF。
    let mut s = TcpStream::connect((HOST, PROXY_PORT)).expect("connect proxy");
    s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(8))).unwrap();
    s.write_all(&[0x05, 0x02, 0x01, 0x02]).unwrap();
    let rep = recvn(&mut s, 2, Duration::from_secs(8));
    r.check(
        "#3c 多方法均非 0x00 -> 0xFF",
        rep == vec![0x05, 0xff],
        &format!("reply={rep:02x?}"),
    );
}

fn case4_halfclose(r: &mut Results) {
    // 不回归：半关闭传播。
    let (mut s, _greet, rep) = socks_connect(HALF_PORT, &[0x00], None);
    if rep.len() < 2 || rep[1] != 0 {
        r.check("#4 半关闭", false, &format!("{rep:02x?}"));
        return;
    }
    s.write_all(b"abc").unwrap();
    let _ = s.shutdown(Shutdown::Write);
    let got = recvn(&mut s, 7, Duration::from_secs(15));
    r.check(
        "#4 半关闭传播未回归",
        got == b"eof:abc".to_vec(),
        &format!("got={got:?}"),
    );
}

fn case5_crlf_and_oversize(r: &mut Results) {
    // 不回归：CRLF 注入、超大头仍被拒绝。
    let mut s = TcpStream::connect((HOST, PROXY_PORT)).expect("connect proxy");
    s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(8))).unwrap();
    s.write_all(b"CONNECT 127.0.0.1:45555\r\nX-Evil: 1 HTTP/1.1\r\n\r\n")
        .unwrap();
    let r1 = read_until_close(&mut s, Duration::from_secs(8));
    let ok1 = !find(
        &r1[..r1
            .len()
            .min(r1.iter().position(|&b| b == b'\n').unwrap_or(r1.len()))],
        b"200",
    )
    .is_some();

    let mut s = TcpStream::connect((HOST, PROXY_PORT)).expect("connect proxy");
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(15))).unwrap();
    let mut req = b"CONNECT 127.0.0.1:45555 HTTP/1.1\r\nX: ".to_vec();
    req.extend(std::iter::repeat(b'a').take(400000));
    s.write_all(&req).unwrap();
    let r2 = read_until_close(&mut s, Duration::from_secs(15));
    let ok2 = find(&r2[..r2.len().min(20)], b"200").is_none();
    r.check(
        "#5 CRLF 注入 / 超大头仍被拒绝",
        ok1 && ok2,
        &format!(
            "crlf={:?} oversize={:?}",
            &r1[..r1.len().min(30)],
            &r2[..r2.len().min(30)]
        ),
    );
}

fn case6_concurrency(r: &mut Results) {
    // 不回归：并发隧道。
    let errs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut ths = Vec::new();
    let t0 = Instant::now();
    for i in 0..48u32 {
        let errs = errs.clone();
        ths.push(std::thread::spawn(move || {
            let res: Result<(), String> = (|| {
                let (mut s, _greet, rep) = socks_connect(ECHO_PORT, &[0x00], None);
                if rep.len() < 2 || rep[1] != 0 {
                    return Err(format!("rep={rep:02x?}"));
                }
                let msg = format!("c{i:03}").into_bytes();
                s.write_all(&msg).unwrap();
                let got = recvn(&mut s, msg.len(), Duration::from_secs(15));
                if got != msg {
                    return Err(format!("echo mismatch: {got:?}"));
                }
                Ok(())
            })();
            if let Err(e) = res {
                errs.lock().unwrap().push(format!("c{i:03}: {e}"));
            }
        }));
    }
    for t in ths {
        let _ = t.join();
    }
    let errs = errs.lock().unwrap().clone();
    r.check(
        "#6 48 并发隧道未回归",
        errs.is_empty(),
        &format!(
            "errs={:?} {:.1}s",
            &errs[..errs.len().min(3)],
            t0.elapsed().as_secs_f64()
        ),
    );
}

// ------------------------------ 主流程 ----------------------------------------

#[test]
fn regression() {
    let served: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    spawn_echo_server();
    spawn_halfclose_server();
    spawn_http_server(served.clone());
    assert!(
        wait_listening(&[ECHO_PORT, HALF_PORT, HTTP_PORT], Duration::from_secs(10)),
        "test servers did not start"
    );

    let nodecfg = std::env::temp_dir().join("baidu-t5-proxy-verify-nodes.txt");
    std::fs::write(&nodecfg, format!("{PROXY_PORT} {HOST} {FAKE_PORT}\n")).expect("write nodes");

    let fake = Command::new(env!("CARGO_BIN_EXE_fake-baidu"))
        .arg(HOST)
        .arg(FAKE_PORT.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fake-baidu");
    let proxy = Command::new(env!("CARGO_BIN_EXE_baidu-t5-proxy"))
        .args([
            "--config",
            nodecfg.to_str().unwrap(),
            "--log-level",
            "WARNING",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn baidu-t5-proxy");
    let _children = Children(proxy, fake);

    assert!(
        wait_listening(&[PROXY_PORT], Duration::from_secs(10)),
        "proxy did not start"
    );

    let mut r = Results::default();
    case1_connect_early_data(&mut r);
    case1b_socks_early_data(&mut r);
    case1c_large_early_data(&mut r);
    case7_connect_pipelined_tls_like(&mut r);
    case2_no_crosstalk(&mut r, &served);
    case2b_absolute_uri_still_works(&mut r, &served);
    case2c_big_body(&mut r);
    case3_method_negotiation(&mut r);
    case3b_noauth_ok(&mut r);
    case3c_multi_methods_no_noauth(&mut r);
    case4_halfclose(&mut r);
    case5_crlf_and_oversize(&mut r);
    case6_concurrency(&mut r);

    let total = r.items.len();
    let passed = r.items.iter().filter(|(_, ok, _)| *ok).count();
    println!("\n==== {passed}/{total} passed ====");
    if passed != total {
        for (n, ok, d) in &r.items {
            if !ok {
                println!("  FAILED: {n}  {d}");
            }
        }
    }
    assert_eq!(passed, total, "regression failures");
}
