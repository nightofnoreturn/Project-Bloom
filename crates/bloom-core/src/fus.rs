use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::header::{
    HeaderMap, HeaderValue, AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, COOKIE, USER_AGENT,
};
use reqwest::Client;

use crate::crypto::{decrypt_nonce, md5_hex};
use crate::data::{BinaryFileInfo, ImeiDb, LatestVersionInfo};
use crate::error::{BloomError, Result};
use crate::xml::{
    create_binary_inform, create_binary_init, create_history_request, extract_fus_status,
    parse_binary_inform, parse_history_infos, parse_version_xml,
};

const FUS_API_BASE: &str = "https://neofussvr.sslcs.cdngc.net";
const FUS_CLOUD_BASE: &str = "http://cloud-neofussvr.samsungmobile.com";

/// Protocol flavor used to communicate with Samsung FUS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusMode {
    Modern,
    Legacy,
}

impl FusMode {
    pub fn is_legacy(self) -> bool {
        matches!(self, Self::Legacy)
    }

    pub fn user_agent(self) -> &'static str {
        match self {
            Self::Modern => "SMART 2.0",
            Self::Legacy => "Kiss2.0_FUS",
        }
    }

    fn generate_nonce_endpoint(self) -> &'static str {
        match self {
            Self::Modern => "NF_SmartDownloadGenerateNonce.do",
            Self::Legacy => "NF_DownloadGenerateNonce.do",
        }
    }

    fn binary_inform_endpoint(self) -> &'static str {
        match self {
            Self::Modern => "NF_SmartDownloadBinaryInform.do",
            Self::Legacy => "NF_DownloadBinaryInform.do",
        }
    }

    fn binary_init_endpoint(self) -> &'static str {
        match self {
            Self::Modern => "NF_SmartDownloadBinaryInitForMass.do",
            Self::Legacy => "NF_DownloadBinaryInitForMass.do",
        }
    }

    fn download_endpoint(self) -> &'static str {
        match self {
            Self::Modern => "NF_SmartDownloadBinaryForMass.do",
            Self::Legacy => "NF_DownloadBinaryForMass.do",
        }
    }
}

/// Authenticated URL + headers required by Samsung's CDN for downloading a firmware file.
#[derive(Debug, Clone)]
pub struct PreparedDownload {
    pub url: String,
    pub auth_header: String,
    pub user_agent: &'static str,
}

/// Async client for Samsung Firmware Update Server (FUS).
#[derive(Debug, Clone)]
pub struct FusClient {
    http: Client,
    mode: FusMode,
    nonce: String,
    auth: String,
    cookies: BTreeMap<String, String>,
}

impl FusClient {
    pub fn new(mode: FusMode) -> Result<Self> {
        let http = Client::builder()
            .http1_only()
            .connect_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;

        Ok(Self {
            http,
            mode,
            nonce: String::new(),
            auth: String::new(),
            cookies: BTreeMap::new(),
        })
    }

    pub fn with_client(http: Client, mode: FusMode) -> Self {
        Self {
            http,
            mode,
            nonce: String::new(),
            auth: String::new(),
            cookies: BTreeMap::new(),
        }
    }

    pub fn mode(&self) -> FusMode {
        self.mode
    }

    pub fn http(&self) -> &Client {
        &self.http
    }

    pub fn download_url(&self, remote_path: &str, file_name: &str) -> String {
        format!(
            "{FUS_CLOUD_BASE}/{}?file={remote_path}{file_name}",
            self.mode.download_endpoint()
        )
    }

    pub async fn ensure_nonce(&mut self) -> Result<&str> {
        if self.nonce.is_empty() {
            self.generate_nonce().await?;
        }
        Ok(&self.nonce)
    }

    pub async fn generate_nonce(&mut self) -> Result<()> {
        let endpoint = self.mode.generate_nonce_endpoint();
        let (status, body) = self.send_fus_request(endpoint, "", None).await?;
        if self.nonce.is_empty() || self.auth.is_empty() {
            let preview: String = body.chars().take(300).collect();
            return Err(BloomError::Crypto(format!(
                "Samsung FUS did not return a valid NONCE header (HTTP {status}): {preview}"
            )));
        }
        Ok(())
    }

    pub fn build_auth_header(&self, signature: Option<&str>) -> String {
        if let Some(sig) = signature.filter(|s| !s.trim().is_empty()) {
            let a = md5_hex(format!("auth:{}:00000001", self.nonce).as_bytes());
            let b = md5_hex(format!("interface:{sig}").as_bytes());
            let sig_hash = md5_hex(format!("{a}:FUS:{b}").as_bytes());
            format!(
                "FUS nonce=\"{}\", signature=\"{sig_hash}\", nc=\"00000001\", type=\"auth\", realm=\"interface\"",
                self.nonce
            )
        } else {
            format!(
                "FUS nonce=\"{}\", signature=\"{}\", nc=\"\", type=\"\", realm=\"\"",
                self.nonce, self.auth
            )
        }
    }

    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn consume_response_headers(&mut self, headers: &HeaderMap) {
        if let Some(raw_nonce) = headers.get("nonce").and_then(|v| v.to_str().ok()) {
            let truncated: String = raw_nonce.trim().chars().take(16).collect();
            if let Ok(new_auth) = decrypt_nonce(&truncated) {
                self.nonce = truncated;
                self.auth = new_auth;
            }
        }

        for val in headers.get_all("set-cookie") {
            if let Ok(cookie_str) = val.to_str() {
                let pair = cookie_str.split(';').next().unwrap_or("").trim();
                if let Some((name, value)) = pair.split_once('=') {
                    let name = name.trim();
                    let value = value.trim();
                    if !name.is_empty() {
                        self.cookies.insert(name.to_owned(), value.to_owned());
                    }
                }
            }
        }
    }

    async fn post_raw(
        &mut self,
        endpoint: &str,
        body_data: &str,
        signature: Option<&str>,
    ) -> Result<(u16, String)> {
        if self.nonce.is_empty() {
            self.generate_nonce().await?;
        }
        self.send_fus_request(endpoint, body_data, signature).await
    }

    async fn send_fus_request(
        &mut self,
        endpoint: &str,
        body_data: &str,
        signature: Option<&str>,
    ) -> Result<(u16, String)> {
        let auth_v = self.build_auth_header(signature);
        let url = format!("{FUS_API_BASE}/{endpoint}");

        let mut req = self
            .http
            .post(url)
            .header(AUTHORIZATION, auth_v)
            .header(USER_AGENT, self.mode.user_agent())
            .header(CONTENT_LENGTH, body_data.len().to_string())
            .body(body_data.to_owned());

        let cookie = self.cookie_header();
        if !cookie.is_empty() {
            req = req.header(COOKIE, cookie);
        }

        let response = req.send().await?;
        let status_code = response.status().as_u16();
        let headers = response.headers().clone();
        let text = response.text().await?;

        self.consume_response_headers(&headers);

        Ok((status_code, text))
    }

    /// Fetch latest firmware version via `SmartHistory.do`, falling back to public `version.xml`.
    pub async fn get_latest_version(
        &mut self,
        model: &str,
        region: &str,
    ) -> Result<LatestVersionInfo> {
        let model = model.trim().to_ascii_uppercase();
        let region = region.trim().to_ascii_uppercase();

        if self.mode == FusMode::Modern {
            if let Ok(history) = self.fetch_smart_history(&model, &region).await {
                if let Some(latest) = history.last() {
                    if !latest.sw_version.is_empty() {
                        return Ok(LatestVersionInfo {
                            version_code: latest.sw_version.clone(),
                            android_version: latest.os_name.clone().unwrap_or_default(),
                            raw_output: String::new(),
                        });
                    }
                }
            }
        }

        self.fetch_version_xml(&model, &region).await
    }

    pub async fn fetch_smart_history(
        &mut self,
        model: &str,
        region: &str,
    ) -> Result<Vec<crate::data::SmartBinaryInfo>> {
        self.ensure_nonce().await?;
        let req_xml = create_history_request(model, region);
        let (status, body) = self
            .post_raw("SmartHistory.do", &req_xml, Some(model))
            .await?;

        if status == 401 || extract_fus_status(&body).as_deref() == Some("401") {
            self.generate_nonce().await?;
            let (_, retry_body) = self
                .post_raw("SmartHistory.do", &req_xml, Some(model))
                .await?;
            return Ok(parse_history_infos(&retry_body));
        }

        Ok(parse_history_infos(&body))
    }

    pub async fn fetch_version_xml_raw(&self, model: &str, region: &str) -> Result<Option<String>> {
        let url = format!("https://fota-cloud-dn.ospserver.net:443/firmware/{region}/{model}/version.xml");
        let response = self
            .http
            .get(url)
            .header(USER_AGENT, "Kiss2.0_FUS")
            .send()
            .await?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let bytes = response.bytes().await?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    async fn fetch_version_xml(&self, model: &str, region: &str) -> Result<LatestVersionInfo> {
        let url = format!("https://fota-cloud-dn.ospserver.net:443/firmware/{region}/{model}/version.xml");
        let response = self
            .http
            .get(url)
            .header(USER_AGENT, "Kiss2.0_FUS")
            .send()
            .await?;
        let body = response.text().await?;
        parse_version_xml(&body)
    }

    /// Retrieve binary metadata for a specific firmware version, trying IMEIs when needed.
    pub async fn retrieve_binary_info(
        &mut self,
        fw: &str,
        model: &str,
        region: &str,
        imei_list: &[String],
        allow_mismatch: bool,
    ) -> Result<BinaryFileInfo> {
        let fw_upper = fw.trim().to_ascii_uppercase();
        let model = model.trim().to_ascii_uppercase();
        let region = region.trim().to_ascii_uppercase();
        let legacy = self.mode.is_legacy();

        let fallback = [String::new()];
        let candidates: &[String] = if imei_list.is_empty() {
            &fallback
        } else {
            imei_list
        };

        let mut last_err: Option<BloomError> = None;

        for (idx, imei) in candidates.iter().enumerate() {
            if idx > 0 && idx % 10 == 0 {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }

            for attempt in 0..2 {
                let nonce = self.ensure_nonce().await?.to_owned();
                let req_xml = create_binary_inform(
                    &fw_upper,
                    &model,
                    &region,
                    &nonce,
                    imei,
                    legacy,
                );

                let endpoint = self.mode.binary_inform_endpoint();
                let (http_status, body) = self.post_raw(endpoint, &req_xml, None).await?;

                if http_status == 401 || extract_fus_status(&body).as_deref() == Some("401") {
                    if attempt == 0 {
                        self.generate_nonce().await?;
                        continue;
                    }
                    return Err(BloomError::Unauthorized);
                }

                if !(200..300).contains(&http_status) {
                    return Err(BloomError::HttpStatus {
                        status: http_status,
                        body,
                    });
                }

                match parse_binary_inform(&body, &fw_upper, &model, &region, allow_mismatch) {
                    Ok(info) => return Ok(info),
                    Err(BloomError::InvalidImeiOrSerial) => {
                        last_err = Some(BloomError::InvalidImeiOrSerial);
                        break; // try next IMEI
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        Err(last_err.unwrap_or(BloomError::InvalidImeiOrSerial))
    }

    /// Call `BINARY_INIT` and return the CDN URL + fresh auth headers needed to download.
    pub async fn prepare_download(
        &mut self,
        info: &BinaryFileInfo,
        region: &str,
        force_refresh_nonce: bool,
    ) -> Result<PreparedDownload> {
        if force_refresh_nonce || self.nonce.is_empty() {
            self.generate_nonce().await?;
        }

        let legacy = self.mode.is_legacy();
        let endpoint = self.mode.binary_init_endpoint();

        for attempt in 0..2 {
            let nonce = self.ensure_nonce().await?.to_owned();
            let req_xml = create_binary_init(
                &info.file_name,
                &nonce,
                info.fw_ver.as_deref(),
                info.model_type.as_deref(),
                region,
                legacy,
            );
            let (http_status, body) = self.post_raw(endpoint, &req_xml, None).await?;

            if http_status == 401 || extract_fus_status(&body).as_deref() == Some("401") {
                if attempt == 0 {
                    self.generate_nonce().await?;
                    continue;
                }
                return Err(BloomError::Unauthorized);
            }
            break;
        }

        Ok(PreparedDownload {
            url: self.download_url(&info.path, &info.file_name),
            auth_header: self.build_auth_header(None),
            user_agent: self.mode.user_agent(),
        })
    }

    /// Build headers map for CDN HTTP `HEAD`/`GET` requests.
    pub fn cdn_headers(prepared: &PreparedDownload) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&prepared.auth_header)
                .map_err(|e| BloomError::Crypto(format!("invalid auth header: {e}")))?,
        );
        headers.insert(USER_AGENT, HeaderValue::from_static(prepared.user_agent));
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        Ok(headers)
    }
}

/// High-level helper that tries `Modern` first and only falls back to `Legacy` before any bytes are downloaded.
pub async fn resolve_binary_with_fallback(
    fw: &str,
    model: &str,
    region: &str,
    custom_imei: Option<&str>,
    imei_db: &ImeiDb,
    allow_mismatch: bool,
) -> Result<(FusClient, BinaryFileInfo)> {
    let mut modern = FusClient::new(FusMode::Modern)?;
    match modern
        .retrieve_binary_info(fw, model, region, &[], allow_mismatch)
        .await
    {
        Ok(info) => Ok((modern, info)),
        Err(e @ BloomError::VersionMismatch { .. }) => Err(e),
        Err(_) => {
            let mut candidates = Vec::new();
            if let Some(raw) = custom_imei {
                for item in raw.split(['\n', ';']) {
                    let s = item.trim();
                    if !s.is_empty() {
                        candidates.push(s.to_owned());
                    }
                }
            }
            if candidates.is_empty() {
                candidates = imei_db.imeis_for_model(model);
            }

            let mut legacy = FusClient::new(FusMode::Legacy)?;
            let info = legacy
                .retrieve_binary_info(fw, model, region, &candidates, allow_mismatch)
                .await?;
            Ok((legacy, info))
        }
    }
}
