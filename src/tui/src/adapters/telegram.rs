//! Telegram Bot API transport. Never log a request URL: it contains a credential.
//! Automatic HTTP retries and redirects are disabled; the journal owns retries.
use reqwest::{
    Url,
    blocking::{Client, Response, multipart},
    redirect, retry,
};
use serde_json::{Value, json};
use std::{io::Read, time::Duration};

pub const JSON_LIMIT: usize = 4 * 1024 * 1024;
pub const FILE_LIMIT: usize = 20 * 1024 * 1024;
pub type Result<T> = std::result::Result<T, Error>;

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
            Self::Invalid(reason) => f.write_str(reason),
            Self::NotSent => f.write_str("Telegram connection failed before submission"),
            Self::RateLimited(_) => f.write_str("Telegram rate limit; retry is scheduled"),
            Self::Rejected(code) => write!(f, "Telegram rejected the request ({code})"),
            Self::Uncertain => {
                f.write_str("Telegram outcome is unknown; do not automatically resend")
            }
        }
    }
}
impl std::error::Error for Error {}

pub trait Api {
    fn get_me(&mut self) -> Result<Value>;
    fn typing(&mut self, _chat: i64) -> Result<()> {
        Err(Error::Invalid("Telegram typing adapter unavailable"))
    }
    fn acknowledge(&mut self, _chat: i64, _message: i64) -> Result<()> {
        Err(Error::Invalid("Telegram reaction adapter unavailable"))
    }
    fn updates(&mut self, offset: i64, timeout: u64) -> Result<Vec<Value>>;
    fn send_text(&mut self, chat: i64, text: &str, reply_to: Option<i64>) -> Result<Value>;
    fn edit_text(&mut self, _chat: i64, _message: i64, _text: &str) -> Result<Value> {
        Err(Error::Invalid("Telegram edit adapter unavailable"))
    }
    fn send_menu(
        &mut self,
        _chat: i64,
        _text: &str,
        _reply_to: Option<i64>,
        _keyboard: Value,
    ) -> Result<Value> {
        Err(Error::Invalid("Telegram keyboard adapter unavailable"))
    }
    fn answer_callback(&mut self, _id: &str, _text: &str) -> Result<()> {
        Err(Error::Invalid("Telegram callback adapter unavailable"))
    }
    fn download(&mut self, file_id: &str) -> Result<Vec<u8>>;
    fn send_document(
        &mut self,
        chat: i64,
        name: &str,
        bytes: Vec<u8>,
        caption: &str,
    ) -> Result<Value>;
}

pub struct BotApi {
    client: Client,
    origin: String,
    token: String,
}
impl std::fmt::Debug for BotApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotApi { credentials: [redacted] }")
    }
}
impl BotApi {
    pub fn new(token: &str, origin: &str) -> Result<Self> {
        Self::build(token, origin, false)
    }
    pub fn feedback(token: &str) -> Result<Self> {
        let mut api = Self::new(token, "https://api.telegram.org")?;
        api.client = Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(3))
            .redirect(redirect::Policy::none())
            .retry(retry::never())
            .no_proxy()
            .build()
            .map_err(|_| Error::Invalid("Cannot initialize feedback transport"))?;
        Ok(api)
    }
    fn build(token: &str, origin: &str, local_test: bool) -> Result<Self> {
        let (id, secret) = token
            .split_once(':')
            .ok_or(Error::Invalid("Invalid Telegram token"))?;
        if id.is_empty()
            || !id.bytes().all(|b| b.is_ascii_digit())
            || secret.is_empty()
            || token.len() > 512
            || !secret
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(Error::Invalid("Invalid Telegram token"));
        }
        let url = Url::parse(origin).map_err(|_| Error::Invalid("Invalid Telegram API origin"))?;
        let loopback_test =
            local_test && url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
        if (url.scheme() != "https" && !loopback_test)
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(Error::Invalid(
                "Telegram API requires an HTTPS origin without credentials or a path",
            ));
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .redirect(redirect::Policy::none())
            .retry(retry::never())
            .referer(false)
            .no_proxy()
            .build()
            .map_err(|_| Error::Invalid("Cannot initialize Telegram transport"))?;
        Ok(Self {
            client,
            origin: origin.trim_end_matches('/').into(),
            token: token.into(),
        })
    }
    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.origin, self.token, method)
    }
    fn call(&self, method: &str, params: &Value) -> Result<Value> {
        let response = self
            .client
            .post(self.url(method))
            .json(params)
            .send()
            .map_err(network)?;
        envelope(response)
    }
}
fn network(error: reqwest::Error) -> Error {
    if error.is_connect() || error.is_builder() {
        Error::NotSent
    } else {
        Error::Uncertain
    }
}
fn bounded(response: Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        return Err(Error::Uncertain);
    }
    let mut bytes = Vec::new();
    response
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Uncertain)?;
    if bytes.len() > limit {
        return Err(Error::Uncertain);
    }
    Ok(bytes)
}
fn envelope(response: Response) -> Result<Value> {
    let status = response.status();
    let body: Value =
        serde_json::from_slice(&bounded(response, JSON_LIMIT)?).map_err(|_| Error::Uncertain)?;
    if status.is_success() && body["ok"] == true {
        return body.get("result").cloned().ok_or(Error::Uncertain);
    }
    // Only an explicit API rejection is proof no operation was accepted.
    if body["ok"] == false {
        let code = body["error_code"].as_u64().unwrap_or(0);
        if code == 429
            && let Some(delay) = body["parameters"]["retry_after"].as_u64()
        {
            return Err(Error::RateLimited(delay.clamp(1, 86_400)));
        }
        if (400..500).contains(&code) {
            return Err(Error::Rejected(code as u16));
        }
    }
    Err(Error::Uncertain)
}
fn message_receipt(value: Value, chat: i64) -> Result<Value> {
    if value["chat"]["id"].as_i64() != Some(chat)
        || value["message_id"].as_i64().is_none_or(|id| id <= 0)
    {
        return Err(Error::Uncertain);
    }
    // Persist only correlation fields, never echo server-controlled text.
    Ok(json!({"chat_id":chat,"message_id":value["message_id"]}))
}
fn valid_chat(chat: i64) -> Result<()> {
    if chat == 0 {
        Err(Error::Invalid("Invalid Telegram chat"))
    } else {
        Ok(())
    }
}
fn validate_keyboard(keyboard: &Value) -> Result<()> {
    let rows = keyboard
        .as_array()
        .filter(|rows| !rows.is_empty() && rows.len() <= 20)
        .ok_or(Error::Invalid("Invalid Telegram keyboard"))?;
    for row in rows {
        let buttons = row
            .as_array()
            .filter(|buttons| !buttons.is_empty() && buttons.len() <= 4)
            .ok_or(Error::Invalid("Invalid Telegram keyboard row"))?;
        for button in buttons {
            let object = button
                .as_object()
                .filter(|o| {
                    o.len() == 2 && o.contains_key("text") && o.contains_key("callback_data")
                })
                .ok_or(Error::Invalid("Only opaque control callbacks are allowed"))?;
            let text = object["text"]
                .as_str()
                .filter(|s| {
                    !s.is_empty() && s.chars().count() <= 64 && !s.chars().any(char::is_control)
                })
                .ok_or(Error::Invalid("Invalid Telegram button label"))?;
            let data = object["callback_data"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 64 && !s.chars().any(char::is_control))
                .ok_or(Error::Invalid("Invalid Telegram callback data"))?;
            let _ = (text, data);
        }
    }
    Ok(())
}
impl Api for BotApi {
    // https://core.telegram.org/bots/api#editmessagetext
    fn edit_text(&mut self, chat: i64, message: i64, text: &str) -> Result<Value> {
        valid_chat(chat)?;
        if message <= 0
            || text.is_empty()
            || text.encode_utf16().count() > 4096
            || text.contains('\0')
        {
            return Err(Error::Invalid("Invalid Telegram message edit"));
        }
        let receipt = message_receipt(
            self.call(
                "editMessageText",
                &json!({"chat_id":chat,"message_id":message,"text":text}),
            )?,
            chat,
        )?;
        if receipt["message_id"].as_i64() != Some(message) {
            return Err(Error::Uncertain);
        }
        Ok(receipt)
    }
    fn get_me(&mut self) -> Result<Value> {
        let me = self.call("getMe", &json!({}))?;
        if me["is_bot"] != true || me["id"].as_i64().is_none_or(|id| id <= 0) {
            return Err(Error::Uncertain);
        }
        Ok(me)
    }
    fn updates(&mut self, offset: i64, timeout: u64) -> Result<Vec<Value>> {
        if offset < 0 || timeout > 50 {
            return Err(Error::Invalid("Invalid Telegram polling parameters"));
        }
        let result=self.call("getUpdates",&json!({"offset":offset,"timeout":timeout,"limit":100,"allowed_updates":["message","callback_query"]}))?;
        let updates = result
            .as_array()
            .filter(|rows| rows.len() <= 100)
            .ok_or(Error::Uncertain)?;
        if updates.iter().any(|u| {
            !u["update_id"]
                .as_i64()
                .is_some_and(|id| (0..i64::MAX).contains(&id))
        }) {
            return Err(Error::Uncertain);
        }
        Ok(updates.clone())
    }
    fn typing(&mut self, chat: i64) -> Result<()> {
        valid_chat(chat)?;
        if self.call("sendChatAction", &json!({"chat_id":chat,"action":"typing"}))? != true {
            return Err(Error::Uncertain);
        }
        Ok(())
    }
    fn acknowledge(&mut self, chat: i64, message: i64) -> Result<()> {
        valid_chat(chat)?;
        if message <= 0 {
            return Err(Error::Invalid("Invalid reaction message"));
        }
        if self.call("setMessageReaction", &json!({"chat_id":chat,"message_id":message,"reaction":[{"type":"emoji","emoji":"👀"}],"is_big":false}))? != true { return Err(Error::Uncertain); }
        Ok(())
    }
    fn send_text(&mut self, chat: i64, text: &str, reply_to: Option<i64>) -> Result<Value> {
        valid_chat(chat)?;
        if text.is_empty()
            || text.chars().count() > 4096
            || text.contains('\0')
            || reply_to.is_some_and(|id| id <= 0)
        {
            return Err(Error::Invalid("Invalid Telegram message"));
        }
        let mut params = json!({"chat_id":chat,"text":text});
        if let Some(id) = reply_to {
            params["reply_parameters"] =
                json!({"message_id":id,"allow_sending_without_reply":false});
        }
        message_receipt(self.call("sendMessage", &params)?, chat)
    }
    fn send_menu(
        &mut self,
        chat: i64,
        text: &str,
        reply_to: Option<i64>,
        keyboard: Value,
    ) -> Result<Value> {
        valid_chat(chat)?;
        if text.is_empty()
            || text.encode_utf16().count() > 4096
            || text.contains('\0')
            || reply_to.is_some_and(|id| id <= 0)
        {
            return Err(Error::Invalid("Invalid Telegram menu text"));
        }
        validate_keyboard(&keyboard)?;
        let mut params =
            json!({"chat_id":chat,"text":text,"reply_markup":{"inline_keyboard":keyboard}});
        if let Some(id) = reply_to {
            params["reply_parameters"] =
                json!({"message_id":id,"allow_sending_without_reply":false});
        }
        message_receipt(self.call("sendMessage", &params)?, chat)
    }
    fn answer_callback(&mut self, id: &str, text: &str) -> Result<()> {
        if id.is_empty()
            || id.len() > 256
            || id.chars().any(char::is_control)
            || text.encode_utf16().count() > 180
            || text.contains('\0')
        {
            return Err(Error::Invalid("Invalid Telegram callback acknowledgement"));
        }
        if self.call(
            "answerCallbackQuery",
            &json!({"callback_query_id":id,"text":text,"cache_time":0}),
        )? != true
        {
            return Err(Error::Uncertain);
        }
        Ok(())
    }
    fn download(&mut self, file_id: &str) -> Result<Vec<u8>> {
        if file_id.is_empty() || file_id.len() > 1024 || file_id.chars().any(char::is_control) {
            return Err(Error::Invalid("Invalid Telegram file identity"));
        }
        let file = self.call("getFile", &json!({"file_id":file_id}))?;
        if file["file_id"].as_str() != Some(file_id)
            || file["file_size"]
                .as_u64()
                .is_some_and(|size| size > FILE_LIMIT as u64)
        {
            return Err(Error::Invalid("Telegram file identity or size is invalid"));
        }
        let path = file["file_path"].as_str().ok_or(Error::Uncertain)?;
        if path.len() > 4096
            || path.split('/').any(|part| {
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        {
            return Err(Error::Invalid("Invalid Telegram file path"));
        }
        let response = self
            .client
            .get(format!("{}/file/bot{}/{}", self.origin, self.token, path))
            .send()
            .map_err(network)?;
        if !response.status().is_success() {
            return Err(Error::Uncertain);
        }
        bounded(response, FILE_LIMIT)
    }
    fn send_document(
        &mut self,
        chat: i64,
        name: &str,
        bytes: Vec<u8>,
        caption: &str,
    ) -> Result<Value> {
        valid_chat(chat)?;
        if bytes.len() > FILE_LIMIT
            || name.is_empty()
            || name.len() > 255
            || name.chars().any(|c| c.is_control() || "/\\\"".contains(c))
            || caption.chars().count() > 1024
        {
            return Err(Error::Invalid(
                "Invalid Telegram document or size exceeds 20 MiB",
            ));
        }
        let form = multipart::Form::new()
            .text("chat_id", chat.to_string())
            .text("caption", caption.to_owned())
            .part(
                "document",
                multipart::Part::bytes(bytes).file_name(name.to_owned()),
            );
        let response = self
            .client
            .post(self.url("sendDocument"))
            .multipart(form)
            .send()
            .map_err(network)?;
        message_receipt(envelope(response)?, chat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
    };
    fn server(status: &str, body: Value) -> (BotApi, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let status = status.to_owned();
        let join = thread::spawn(move || {
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
                if line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse::<usize>().unwrap();
                }
                request.push_str(&line);
            }
            let mut payload = vec![0; length];
            reader.read_exact(&mut payload).unwrap();
            request.push_str(&String::from_utf8_lossy(&payload));
            let body = body.to_string();
            write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            request
        });
        (BotApi::build("123:fixture", &origin, true).unwrap(), join)
    }
    #[test]
    fn feedback_uses_native_actions_without_sending_text() {
        let (mut api, join) = server("200 OK", json!({"ok":true,"result":true}));
        api.typing(42).unwrap();
        let request = join.join().unwrap();
        assert!(request.contains("/sendChatAction"));
        let body: Value = serde_json::from_str(request.lines().last().unwrap())
            .unwrap_or_else(|_| serde_json::from_str(request.lines().last().unwrap()).unwrap());
        assert_eq!(body, json!({"chat_id":42,"action":"typing"}));
        let (mut api, join) = server("200 OK", json!({"ok":true,"result":true}));
        api.acknowledge(42, 7).unwrap();
        let request = join.join().unwrap();
        assert!(request.contains("/setMessageReaction"));
        assert!(request.contains("👀"));
        assert!(request.contains("\"message_id\":7"));
        let (mut api, join) = server("200 OK", json!({"ok":true,"result":false}));
        assert_eq!(api.typing(42), Err(Error::Uncertain));
        join.join().unwrap();
    }
    #[test]
    fn text_submission_requires_correlated_receipt() {
        let (mut api, join) = server(
            "200 OK",
            json!({"ok":true,"result":{"message_id":7,"chat":{"id":42},"text":"private"}}),
        );
        assert_eq!(
            api.send_text(42, "hello", Some(5)).unwrap(),
            json!({"chat_id":42,"message_id":7})
        );
        let request = join.join().unwrap();
        assert!(request.contains("POST /bot123:fixture/sendMessage"));
        assert!(request.contains("reply_parameters"));
        let (mut api, join) = server(
            "200 OK",
            json!({"ok":true,"result":{"message_id":7,"chat":{"id":99}}}),
        );
        assert_eq!(api.send_text(42, "hello", None), Err(Error::Uncertain));
        join.join().unwrap();
    }
    #[test]
    fn errors_are_redacted_and_only_explicit_rate_limit_is_retryable() {
        let (mut api, join) = server(
            "429 Too Many Requests",
            json!({"ok":false,"error_code":429,"description":"sensitive","parameters":{"retry_after":3}}),
        );
        assert_eq!(api.send_text(42, "hello", None), Err(Error::RateLimited(3)));
        join.join().unwrap();
        let (mut api, join) = server(
            "500 Server Error",
            json!({"ok":false,"error_code":500,"description":"sensitive"}),
        );
        let error = api.send_text(42, "hello", None).unwrap_err();
        assert_eq!(error, Error::Uncertain);
        assert!(!error.to_string().contains("sensitive"));
        join.join().unwrap();
        assert!(!format!("{api:?}").contains("fixture"));
    }
    #[test]
    fn refuses_insecure_origins_and_path_injection() {
        for origin in [
            "http://example.org",
            "https://user@example.org",
            "https://example.org/path",
            "https://example.org?x=1",
        ] {
            assert!(BotApi::new("123:fixture", origin).is_err());
        }
        assert!(BotApi::new("123:bad/secret", "https://api.telegram.org").is_err());
        let (mut api, join) = server(
            "200 OK",
            json!({"ok":true,"result":{"file_id":"file","file_path":"../private"}}),
        );
        assert!(matches!(api.download("file"), Err(Error::Invalid(_))));
        join.join().unwrap();
    }
    #[test]
    fn polls_only_supported_update_types_and_bounds_offset() {
        let (mut api, join) = server("200 OK", json!({"ok":true,"result":[{"update_id":12}]}));
        assert_eq!(api.updates(12, 50).unwrap().len(), 1);
        let request = join.join().unwrap();
        assert!(request.contains("callback_query"));
        assert!(!request.contains("edited_message"));
        assert!(api.updates(-1, 50).is_err());
    }
    #[test]
    fn inline_controls_and_acknowledgements_validate_wire_contract() {
        let (mut api, join) = server(
            "200 OK",
            json!({"ok":true,"result":{"message_id":7,"chat":{"id":42}}}),
        );
        let keyboard =
            json!([[{"text":"Confirm","callback_data":"dc:0123456789abcdef0123456789abcdef"}]]);
        api.send_menu(42, "Review action", None, keyboard).unwrap();
        let request = join.join().unwrap();
        assert!(request.contains("inline_keyboard"));
        let (mut api, join) = server("200 OK", json!({"ok":true,"result":true}));
        api.answer_callback("query-id", "Recorded").unwrap();
        assert!(join.join().unwrap().contains("answerCallbackQuery"));
        assert!(
            validate_keyboard(&json!([[{"text":"Open","url":"https://example.com"}]] )).is_err()
        );
        assert!(
            validate_keyboard(&json!([[{"text":"Confirm","callback_data":"x".repeat(65)}]]))
                .is_err()
        );
    }
}
