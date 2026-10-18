//! Bounded documented Hindsight HTTP API. No redirects, implicit retry or
//! credential-bearing errors. Async retain receipts mean accepted, not finished.
//! https://docs.hindsight.vectorize.io/api-reference/retain-memories/
//! https://docs.hindsight.vectorize.io/api-reference/recall-memories/
use reqwest::{Url, blocking::Client, redirect, retry};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io::Read, time::Duration};
const LIMIT: u64 = 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    NotSent,
    RateLimited(u64),
    Rejected(u16),
    Uncertain,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(s) => f.write_str(s),
            Self::NotSent => f.write_str("Memory service connection failed before submission"),
            Self::RateLimited(_) => f.write_str("Memory service rate limited the request"),
            Self::Rejected(code) => write!(f, "Memory service rejected request ({code})"),
            Self::Uncertain => f.write_str("Memory request outcome is unknown"),
        }
    }
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    pub bank: String,
    pub operation_id: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Pending,
    Processing,
    Completed,
    Failed,
    Cancelled,
    NotFound,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub bank: String,
    pub operation_id: String,
    pub state: OperationState,
}
pub trait StatusApi {
    fn operation(&mut self, bank: &str, operation: &str) -> Result<Operation>;
}
pub trait Api {
    fn recall(&mut self, bank: &str, query: &str) -> Result<Vec<Memory>>;
    fn retain(
        &mut self,
        bank: &str,
        document: &str,
        content: &str,
        operation: &str,
    ) -> Result<Submission>;
}
pub struct Http {
    client: Client,
    endpoint: Url,
    key: Option<String>,
}
impl std::fmt::Debug for Http {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hindsight { configuration: [private] }")
    }
}
pub fn endpoint(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| Error::Invalid("Invalid memory service endpoint"))?;
    let local = url.host_str().is_some_and(|h| {
        h == "localhost"
            || h.trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !(url.scheme() == "https" || url.scheme() == "http" && local)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Invalid(
            "Memory endpoint requires HTTPS or loopback HTTP without embedded credentials",
        ));
    }
    Ok(url)
}
impl Http {
    pub fn new(base: &str, key: Option<String>, timeout: Duration) -> Result<Self> {
        let endpoint = endpoint(base)?;
        if timeout.is_zero()
            || timeout > Duration::from_secs(10)
            || key.as_ref().is_some_and(|k| {
                k.is_empty()
                    || k.len() > 4096
                    || k.chars().any(char::is_whitespace)
                    || k.chars().any(char::is_control)
            })
        {
            return Err(Error::Invalid("Invalid memory credentials or timeout"));
        }
        let client = Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(3)))
            .redirect(redirect::Policy::none())
            .retry(retry::never())
            .no_proxy()
            .build()
            .map_err(|_| Error::Invalid("Cannot initialize memory HTTP client"))?;
        Ok(Self {
            client,
            endpoint,
            key,
        })
    }
    fn call(&self, bank: &str, recall: bool, body: Value) -> Result<Value> {
        valid_id(bank)?;
        let mut url = self.endpoint.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| Error::Invalid("Invalid memory endpoint"))?;
            path.pop_if_empty()
                .extend(["v1", "default", "banks", bank, "memories"]);
            if recall {
                path.push("recall");
            }
        }
        self.request(self.client.post(url).json(&body))
    }
    fn request(&self, mut request: reqwest::blocking::RequestBuilder) -> Result<Value> {
        if let Some(key) = &self.key {
            request = request.bearer_auth(key);
        }
        let response = request.send().map_err(|e| {
            if e.is_connect() {
                Error::NotSent
            } else {
                Error::Uncertain
            }
        })?;
        let status = response.status();
        if status.as_u16() == 429 {
            let seconds = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(5)
                .clamp(1, 86400);
            return Err(Error::RateLimited(seconds));
        }
        if status.is_client_error() {
            return Err(Error::Rejected(status.as_u16()));
        }
        if !status.is_success() || response.content_length().is_some_and(|n| n > LIMIT) {
            return Err(Error::Uncertain);
        }
        let mut bytes = Vec::new();
        response
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Uncertain)?;
        if bytes.len() as u64 > LIMIT {
            return Err(Error::Uncertain);
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::Uncertain)
    }
}
impl StatusApi for Http {
    fn operation(&mut self, bank: &str, operation: &str) -> Result<Operation> {
        valid_id(bank)?;
        valid_id(operation)?;
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| Error::Invalid("Invalid memory endpoint"))?
            .pop_if_empty()
            .extend(["v1", "default", "banks", bank, "operations", operation]);
        // Never request the original task payload, provider errors or debugging data.
        url.query_pairs_mut()
            .append_pair("include_payload", "false");
        let body = self.request(self.client.get(url))?;
        if body["operation_id"] != operation
            || body.get("bank_id").is_some_and(|id| id != bank)
            || body
                .get("operation_type")
                .filter(|v| !v.is_null())
                .is_some_and(|kind| kind != "retain")
        {
            return Err(Error::Uncertain);
        }
        let state = serde_json::from_value(body["status"].clone()).map_err(|_| Error::Uncertain)?;
        Ok(Operation {
            bank: bank.into(),
            operation_id: operation.into(),
            state,
        })
    }
}
fn valid_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(char::is_control)
        || matches!(value, "." | "..")
    {
        Err(Error::Invalid("Invalid memory identity"))
    } else {
        Ok(())
    }
}
impl Api for Http {
    fn recall(&mut self, bank: &str, query: &str) -> Result<Vec<Memory>> {
        if query.trim().is_empty() || query.len() > 8192 || query.contains('\0') {
            return Err(Error::Invalid("Invalid memory query"));
        }
        let body = self.call(
            bank,
            true,
            json!({"query":query,"budget":"low","max_tokens":900}),
        )?;
        let rows = body["results"]
            .as_array()
            .filter(|v| v.len() <= 1000)
            .ok_or(Error::Uncertain)?;
        let mut memories = Vec::new();
        let mut remaining = 8192;
        for row in rows.iter().take(5) {
            let id = row["id"].as_str().ok_or(Error::Uncertain)?;
            valid_id(id).map_err(|_| Error::Uncertain)?;
            let text = row["text"]
                .as_str()
                .filter(|t| !t.contains('\0'))
                .ok_or(Error::Uncertain)?;
            let mut end = text.len().min(remaining);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            if end == 0 {
                break;
            }
            memories.push(Memory {
                id: id.into(),
                text: text[..end].into(),
            });
            remaining -= end;
        }
        Ok(memories)
    }
    fn retain(
        &mut self,
        bank: &str,
        document: &str,
        content: &str,
        operation: &str,
    ) -> Result<Submission> {
        valid_id(document)?;
        valid_id(operation)?;
        if content.trim().is_empty() || content.len() > 64 * 1024 || content.contains('\0') {
            return Err(Error::Invalid("Invalid memory content"));
        }
        let body=self.call(bank,false,json!({"async":true,"operation_id":operation,"items":[{"content":content,"document_id":document,"context":"Daedalus lifecycle hook"}]}))?;
        let id = body["operation_id"].as_str().ok_or(Error::Uncertain)?;
        if body["success"] != true
            || body["async"] != true
            || body["bank_id"] != bank
            || body["items_count"] != 1
            || id != operation
        {
            return Err(Error::Uncertain);
        }
        Ok(Submission {
            bank: bank.into(),
            operation_id: id.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
    };
    fn server(
        status: &str,
        body: &str,
        extra: &str,
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
            body.len()
        );
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                request.push_str(&line);
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse::<usize>().unwrap();
                }
            }
            assert!(length < 128 * 1024);
            let mut content = vec![0; length];
            reader.read_exact(&mut content).unwrap();
            request.push_str(std::str::from_utf8(&content).unwrap());
            let _ = tx.send(request);
            stream.write_all(response.as_bytes()).unwrap();
        });
        (address, rx, handle)
    }
    #[test]
    fn recall_encodes_bank_as_one_path_segment_and_bounds_context() {
        let body = json!({"results":[{"id":"memory-id","text":"Remember the project decision"}]})
            .to_string();
        let (endpoint, request, server) = server("200 OK", &body, "");
        let mut api = Http::new(
            &endpoint,
            Some("fixture-key".into()),
            Duration::from_secs(2),
        )
        .unwrap();
        let result = api.recall("bank/with space", "project decision").unwrap();
        assert_eq!(result[0].text, "Remember the project decision");
        let request = request.recv().unwrap();
        assert!(request.starts_with("POST /v1/default/banks/bank%2Fwith%20space/memories/recall "));
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer fixture-key")
        );
        assert!(request.contains("\"budget\":\"low\""));
        server.join().unwrap();
        assert!(!format!("{api:?}").contains("fixture-key"));
    }
    #[test]
    fn operation_status_is_correlated_bounded_and_does_not_return_private_payloads() {
        for state in [
            "pending",
            "processing",
            "completed",
            "failed",
            "cancelled",
            "not_found",
        ] {
            let body = json!({"operation_id":"op/1","status":state,"operation_type":"retain","task_payload":{"content":"private source"},"error_message":"private error"});
            let (endpoint, request, server) = server("200 OK", &body.to_string(), "");
            let mut api = Http::new(&endpoint, None, Duration::from_secs(2)).unwrap();
            let observed = api.operation("bank/one", "op/1").unwrap();
            assert_eq!(serde_json::to_value(observed.state).unwrap(), state);
            assert!(
                !serde_json::to_string(&observed)
                    .unwrap()
                    .contains("private")
            );
            assert!(request.recv().unwrap().starts_with(
                "GET /v1/default/banks/bank%2Fone/operations/op%2F1?include_payload=false "
            ));
            server.join().unwrap();
        }
        for body in [
            json!({"operation_id":"wrong","status":"completed"}),
            json!({"operation_id":"op","status":"future_state"}),
            json!({"operation_id":"op","status":"completed","operation_type":"refresh_mental_model"}),
            json!({"operation_id":"op","status":"completed","bank_id":"other"}),
        ] {
            let (endpoint, _, server) = server("200 OK", &body.to_string(), "");
            assert_eq!(
                Http::new(&endpoint, None, Duration::from_secs(2))
                    .unwrap()
                    .operation("bank", "op"),
                Err(Error::Uncertain)
            );
            server.join().unwrap();
        }
    }
    #[test]
    fn retain_acceptance_requires_correlated_bank_operation_and_count() {
        let operation = "4c7f73bc-8591-4239-8bda-13cba892df23";
        let body=json!({"success":true,"async":true,"bank_id":"bank","items_count":1,"operation_id":operation}).to_string();
        let (endpoint, request, server) = server("200 OK", &body, "");
        let mut api = Http::new(&endpoint, None, Duration::from_secs(2)).unwrap();
        assert_eq!(
            api.retain("bank", "doc", "Remember this", operation)
                .unwrap()
                .operation_id,
            operation
        );
        let request = request.recv().unwrap();
        assert!(request.contains("\"async\":true"));
        server.join().unwrap();
        for bad in [
            json!({"success":true,"async":true,"bank_id":"other","items_count":1,"operation_id":operation}),
            json!({"success":true,"async":true,"bank_id":"bank","items_count":1,"operation_id":"other"}),
        ] {
            let (endpoint, _, handle) = self::server("200 OK", &bad.to_string(), "");
            let mut api = Http::new(&endpoint, None, Duration::from_secs(2)).unwrap();
            assert_eq!(
                api.retain("bank", "doc", "Remember this", operation),
                Err(Error::Uncertain)
            );
            handle.join().unwrap();
        }
    }
    #[test]
    fn redirect_rejection_and_rate_limit_are_not_retried() {
        for (status, extra, expected) in [
            (
                "302 Found",
                "Location: http://127.0.0.1:1/secret\r\n",
                Error::Uncertain,
            ),
            (
                "429 Too Many Requests",
                "Retry-After: 13\r\n",
                Error::RateLimited(13),
            ),
            ("403 Forbidden", "", Error::Rejected(403)),
        ] {
            let (endpoint, _, handle) = server(status, "{}", extra);
            let mut api = Http::new(&endpoint, None, Duration::from_secs(2)).unwrap();
            assert_eq!(api.recall("bank", "query"), Err(expected));
            handle.join().unwrap();
        }
        for invalid in [
            "http://example.com",
            "https://user:secret@example.com",
            "https://example.com/?secret=value",
            "https://example.com/#secret",
        ] {
            assert!(Http::new(invalid, None, Duration::from_secs(2)).is_err());
        }
    }
}
