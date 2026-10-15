use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Payload {
    pub title: String,
    pub status: String,
    pub duration_ms: u64,
    pub summary: String,
}

pub fn notify_webhooks(webhooks: &[String], payload: &Payload) -> Vec<std::thread::JoinHandle<()>> {
    let json = serde_json::to_string(payload).unwrap_or_else(|err| {
        eprintln!("[webhook] failed to serialize payload: {err}");
        String::new()
    });
    if json.is_empty() {
        return Vec::new();
    }
    webhooks.iter().map(|url| {
        let url = url.clone();
        let json = json.clone();
        std::thread::spawn(move || send_single(&url, &json))
    }).collect()
}

fn send_single(url: &str, json: &str) {
    if let Err(err) = ureq::post(url)
        .set("Content-Type", "application/json")
        .send_string(json)
    {
        eprintln!("[webhook] failed to notify {url}: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};

    fn read_http_request(stream: &mut TcpStream) -> (String, String, String) {
        let mut reader = BufReader::new(&mut *stream);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut headers = String::new();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).unwrap();
            if n == 0 { break; }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            headers.push_str(&line);
            if line == "\r\n" { break; }
        }
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).unwrap();
        (request_line, headers, String::from_utf8(body).unwrap())
    }

    #[test]
    fn test_webhook_posts_expected_json_payload() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/hook");

        let server_thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let result = read_http_request(&mut stream);
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
            result
        });

        let payload = Payload {
            title: "任务A".to_string(),
            status: "success".to_string(),
            duration_ms: 1234,
            summary: "完成摘要".to_string(),
        };

        let handles = notify_webhooks(&[url], &payload);
        for handle in handles {
            handle.join().unwrap();
        }

        let (request_line, headers, body) = server_thread.join().unwrap();
        assert!(request_line.starts_with("POST /hook HTTP/1.1\r\n"));
        assert!(headers.to_lowercase().contains("content-type: application/json"));

        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["title"], "任务A");
        assert_eq!(value["status"], "success");
        assert_eq!(value["duration_ms"], 1234);
        assert_eq!(value["summary"], "完成摘要");
    }

    #[test]
    fn test_webhook_unreachable_url_does_not_panic() {
        let payload = Payload {
            title: "失败通知".to_string(),
            status: "failure".to_string(),
            duration_ms: 0,
            summary: "不应 panic".to_string(),
        };

        let handles = notify_webhooks(&["http://127.0.0.1:1/unreachable".to_string()], &payload);
        for handle in handles {
            handle.join().unwrap();
        }
    }
}
