use roxmltree::{Document, Node};

use crate::crypto::md5_bytes;
use crate::data::{BinaryFileInfo, LatestVersionInfo, SmartBinaryInfo};
use crate::error::{BloomError, Result};

fn escape_xml(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn data_node(tag: &str, value: &str) -> String {
    format!("<{tag}><Data>{}</Data></{tag}>", escape_xml(value))
}

fn text_node(tag: &str, value: &str) -> String {
    format!("<{tag}>{}</{tag}>", escape_xml(value))
}

fn first_descendant<'a, 'input>(
    node: Node<'a, 'input>,
    tag: &str,
) -> Option<Node<'a, 'input>> {
    node.descendants().find(|n| n.is_element() && n.tag_name().name() == tag)
}

fn first_data_by_tag(node: Node<'_, '_>, tag: &str) -> Option<String> {
    let el = first_descendant(node, tag)?;
    let data = first_descendant(el, "Data")?;
    Some(data.text().unwrap_or_default().to_owned())
}

fn first_text_by_tag(node: Node<'_, '_>, tag: &str) -> Option<String> {
    let el = first_descendant(node, tag)?;
    Some(el.text().unwrap_or_default().to_owned())
}

fn first_value_by_tag(node: Node<'_, '_>, tag: &str) -> Option<String> {
    first_data_by_tag(node, tag)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| first_text_by_tag(node, tag).filter(|s| !s.trim().is_empty()))
}

/// Extract `<FUSBody><Results><Status>` if present.
pub fn extract_fus_status(xml_str: &str) -> Option<String> {
    let doc = Document::parse(xml_str).ok()?;
    let body = first_descendant(doc.root(), "FUSBody")?;
    let results = first_descendant(body, "Results")?;
    first_text_by_tag(results, "Status").map(|s| s.trim().to_owned())
}

/// Compute Samsung's `LOGIC_CHECK` token from an input string and a 16-char nonce/logic value.
pub fn get_logic_check(input: &str, nonce: &str) -> String {
    let bytes = input.as_bytes();
    if bytes.len() < 16 {
        return String::new();
    }
    nonce
        .bytes()
        .map(|b| bytes[(b as usize) & 0x0F] as char)
        .collect()
}

/// Build the XML payload for `SmartHistory.do`.
pub fn create_history_request(model: &str, region: &str) -> String {
    format!(
        "<FUSMsg><FUSHdr>{}{}{}</FUSHdr><FUSBody><Put>{}{}{}</Put></FUSBody></FUSMsg>",
        text_node("ProtoVer", "1"),
        text_node("SessionID", "0"),
        text_node("MsgID", "1"),
        data_node("ACCESS_MODE", "1"),
        data_node("BINARY_MODEL_NAME", model.trim()),
        data_node("BINARY_LOCAL_CODE", region.trim()),
    )
}

/// Build the XML payload for `NF_SmartDownloadBinaryInform.do` or `NF_DownloadBinaryInform.do`.
pub fn create_binary_inform(
    fw: &str,
    model: &str,
    region: &str,
    nonce: &str,
    imei_serial: &str,
    legacy: bool,
) -> String {
    let logic_check = get_logic_check(fw, nonce);
    let mut parts = fw.split('/');
    let pda = parts.next().unwrap_or("");
    let csc = parts.next().unwrap_or("");
    let phone = parts.next().unwrap_or("");
    let data = parts.next().unwrap_or("");

    let mut put = String::new();
    if !legacy {
        put.push_str(&text_node("CmdID", "1"));
        put.push_str(&data_node("REQUEST_TYPE", "2"));
        put.push_str(&data_node("BINARY_SW_VERSION", fw));
        put.push_str(&data_node("DEVICE_SN_NUMBER", ""));
        put.push_str(&data_node("BINARY_LOCAL_CODE", region));
        put.push_str(&data_node("BINARY_MODEL_NAME", model));
    } else {
        put.push_str(&data_node("CLIENT_PRODUCT", "Smart Switch"));
        put.push_str(&data_node("CLIENT_VERSION", "4.3.23123_1"));
        put.push_str(&data_node("DEVICE_IMEI_PUSH", imei_serial.trim()));
        put.push_str(&data_node("DEVICE_FW_VERSION", fw.trim()));
        put.push_str(&data_node("DEVICE_LOCAL_CODE", region.trim()));
        put.push_str(&data_node("DEVICE_AID_CODE", region.trim()));
        put.push_str(&data_node("DEVICE_MODEL_NAME", model.trim()));
        put.push_str(&data_node("DEVICE_CONTENTS_DATA_VERSION", data.trim()));
        put.push_str(&data_node("DEVICE_CSC_CODE2_VERSION", csc.trim()));
        put.push_str(&data_node("DEVICE_PDA_CODE1_VERSION", pda.trim()));
        put.push_str(&data_node("DEVICE_PHONE_FONT_VERSION", phone.trim()));
    }

    put.push_str(&data_node("ACCESS_MODE", if legacy { "2" } else { "1" }));
    put.push_str(&data_node("BINARY_NATURE", "1"));
    put.push_str(&data_node("LOGIC_CHECK", logic_check.trim()));
    put.push_str(
        "<CLIENT_LANGUAGE><Type>String</Type><Type>ISO 3166-1-alpha-3</Type><Data>1033</Data></CLIENT_LANGUAGE>",
    );

    match region {
        "EUX" => {
            put.push_str(&data_node("DEVICE_CC_CODE", "DE"));
            put.push_str(&data_node("MCC_NUM", "262"));
            put.push_str(&data_node("MNC_NUM", "01"));
        }
        "EUY" => {
            put.push_str(&data_node("DEVICE_CC_CODE", "RS"));
            put.push_str(&data_node("MCC_NUM", "220"));
            put.push_str(&data_node("MNC_NUM", "01"));
        }
        _ => {}
    }

    let get_tag = if legacy {
        "<LATEST_FW_VERSION/>"
    } else {
        "<BINARY_SW_VERSION/>"
    };

    format!(
        "<FUSMsg><FUSHdr>{}{}{}</FUSHdr><FUSBody><Put>{}</Put><Get>{}{}</Get></FUSBody></FUSMsg>",
        text_node("ProtoVer", if legacy { "1.0" } else { "1" }),
        text_node("SessionID", "0"),
        text_node("MsgID", "1"),
        put,
        text_node("CmdID", "2"),
        get_tag
    )
}

/// Build the XML payload for `NF_SmartDownloadBinaryInitForMass.do` or `NF_DownloadBinaryInitForMass.do`.
pub fn create_binary_init(
    file_name: &str,
    nonce: &str,
    fw: Option<&str>,
    model_type: Option<&str>,
    region: &str,
    legacy: bool,
) -> String {
    let special = if legacy {
        let stem = file_name.split('.').next().unwrap_or(file_name);
        if stem.is_empty() {
            ""
        } else {
            let rem = 16 % stem.len();
            let start = stem.len().saturating_sub(rem);
            &stem[start..]
        }
    } else if file_name.len() >= 25 {
        &file_name[file_name.len() - 25..file_name.len() - 9]
    } else {
        ""
    };

    let logic_check = get_logic_check(special, nonce);

    let mut hdr = text_node("ProtoVer", if legacy { "1.0" } else { "1" });
    if !legacy {
        hdr.push_str(&text_node("SessionID", "0"));
        hdr.push_str(&text_node("MsgID", "1"));
    }

    let mut put = data_node(if legacy { "BINARY_FILE_NAME" } else { "BINARY_NAME" }, file_name);
    if !legacy {
        if let Some(fw_str) = fw {
            put.push_str(&data_node("BINARY_SW_VERSION", fw_str));
        }
        put.push_str(&data_node("DEVICE_LOCAL_CODE", region));
        if let Some(mt) = model_type {
            put.push_str(&data_node("DEVICE_MODEL_TYPE", mt));
        }
    }
    put.push_str(&data_node("LOGIC_CHECK", &logic_check));

    format!("<FUSMsg><FUSHdr>{hdr}</FUSHdr><FUSBody><Put>{put}</Put></FUSBody></FUSMsg>")
}

/// Parse `SmartHistory.do` XML response into sorted non-beta firmware entries.
pub fn parse_history_infos(xml_str: &str) -> Vec<SmartBinaryInfo> {
    let Ok(doc) = Document::parse(xml_str) else {
        return Vec::new();
    };

    let mut items: Vec<SmartBinaryInfo> = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "BINARY_INFO")
        .filter_map(|info| {
            let sequence = first_data_by_tag(info, "BINARY_SEQUENCE")?
                .trim()
                .parse::<i32>()
                .ok()?;
            let os_name = first_data_by_tag(info, "BINARY_OS_NAME");
            if os_name.as_deref() == Some("Z(Android 99)") {
                return None;
            }
            Some(SmartBinaryInfo {
                sequence,
                model_name: first_data_by_tag(info, "BINARY_MODEL_NAME").unwrap_or_default(),
                sw_version: first_data_by_tag(info, "BINARY_SW_VERSION").unwrap_or_default(),
                local_code: first_data_by_tag(info, "BINARY_LOCAL_CODE").unwrap_or_default(),
                os_name,
                open_date: first_data_by_tag(info, "BINARY_OPEN_DATE"),
            })
        })
        .collect();

    items.sort_by_key(|item| item.sequence);
    items
}

/// Parse `https://fota-cloud-dn.ospserver.net/firmware/{region}/{model}/version.xml`.
pub fn parse_version_xml(xml_str: &str) -> Result<LatestVersionInfo> {
    let doc = Document::parse(xml_str)?;
    let root = doc.root_element();

    if root.tag_name().name() == "Error" {
        let code = first_text_by_tag(root, "Code").unwrap_or_default();
        let message = first_text_by_tag(root, "Message").unwrap_or_default();
        return Err(BloomError::VersionCheck(format!("Code: {code}, Message: {message}")));
    }

    let latest = first_descendant(doc.root(), "firmware")
        .and_then(|n| first_descendant(n, "version"))
        .and_then(|n| first_descendant(n, "latest"));

    let Some(latest_node) = latest else {
        let access_denied = first_text_by_tag(doc.root(), "code").as_deref() == Some("AccessDenied");
        return Err(BloomError::VersionCheck(if access_denied {
            "Invalid CSC / AccessDenied".into()
        } else {
            "No firmware found in version.xml".into()
        }));
    };

    let latest_text = latest_node.text().unwrap_or_default().trim();
    if latest_text.is_empty() {
        let access_denied = first_text_by_tag(doc.root(), "code").as_deref() == Some("AccessDenied");
        return Err(BloomError::VersionCheck(if access_denied {
            "Invalid CSC / AccessDenied".into()
        } else {
            "No latest firmware tag found".into()
        }));
    }

    let mut vc: Vec<String> = latest_text.split('/').map(ToOwned::to_owned).collect();
    if vc.len() == 3 {
        vc.push(vc[0].clone());
    }
    if vc.len() > 2 && vc[2].is_empty() {
        vc[2] = vc[0].clone();
    }

    let android_version = latest_node.attribute("o").unwrap_or("").to_owned();

    Ok(LatestVersionInfo {
        version_code: vc.join("/"),
        android_version,
        raw_output: xml_str.to_owned(),
    })
}

/// Extract `.enc4` decryption key `(md5_bytes, raw_dec_key_string)` from `BINARY_INFORM` XML.
pub fn extract_v4_key(doc: &Document<'_>) -> Option<([u8; 16], String)> {
    let body = first_descendant(doc.root(), "FUSBody")?;
    let results = first_descendant(body, "Results")?;
    let fw_ver = first_data_by_tag(results, "LATEST_FW_VERSION")
        .or_else(|| first_data_by_tag(results, "BINARY_SW_VERSION"))?;

    let put = first_descendant(body, "Put")?;
    let logic_val = first_data_by_tag(put, "LOGIC_VALUE_FACTORY")
        .or_else(|| first_data_by_tag(put, "LOGIC_VALUE_HOME"))?;

    let dec_key = get_logic_check(&fw_ver, &logic_val);
    if dec_key.is_empty() {
        return None;
    }
    Some((md5_bytes(dec_key.as_bytes()), dec_key))
}

fn model_match_score(file_segment: &str, model: &str) -> usize {
    // Real build version tokens (e.g. A075FXXU7DZJ5) always have digits and are >= 6 chars,
    // unlike short component prefixes such as "AP", "BL", "CP", "CSC", or "HOME".
    if file_segment.len() < 6 || !file_segment.bytes().any(|b| b.is_ascii_digit()) {
        return 0;
    }
    let mut best = 0usize;
    let mut current = model;
    while !current.is_empty() && !current.ends_with('-') {
        let model_suffix = current.split('-').nth(1).unwrap_or(current);
        let joined_model = current.replace('-', "");
        if model_suffix.len() >= 2 && file_segment.starts_with(model_suffix) {
            best = best.max(model_suffix.len());
        }
        if joined_model.len() >= 3 && file_segment.starts_with(&joined_model) {
            best = best.max(joined_model.len());
        }
        let Some(last_idx) = current.char_indices().last().map(|(i, _)| i) else {
            break;
        };
        current = &current[..last_idx];
    }
    best
}

fn get_segment_index(file: Option<&str>, model: &str) -> Option<usize> {
    let file = file.filter(|s| !s.trim().is_empty())?;
    file.split('_')
        .enumerate()
        .filter_map(|(idx, seg)| {
            let score = model_match_score(seg, model);
            (score > 0).then_some((idx, score))
        })
        .max_by(|(idx_a, score_a), (idx_b, score_b)| {
            score_a.cmp(score_b).then_with(|| idx_b.cmp(idx_a))
        })
        .map(|(idx, _)| idx)
}

/// Parse `BINARY_INFORM` response XML into [`BinaryFileInfo`].
pub fn parse_binary_inform(
    xml_str: &str,
    fw: &str,
    model: &str,
    region: &str,
    allow_mismatch: bool,
) -> Result<BinaryFileInfo> {
    let doc = Document::parse(xml_str)?;
    let body = first_descendant(doc.root(), "FUSBody")
        .ok_or_else(|| BloomError::FusStatus {
            status: "MISSING_BODY".into(),
            raw: xml_str.to_owned(),
        })?;

    let status = first_descendant(body, "Results")
        .and_then(|r| first_text_by_tag(r, "Status"))
        .unwrap_or_default()
        .trim()
        .to_owned();

    match status.as_str() {
        "F01" => return Err(BloomError::InvalidFirmware),
        "408" => return Err(BloomError::InvalidImeiOrSerial),
        "401" => return Err(BloomError::Unauthorized),
        "200" | "S00" => {}
        _ => {
            return Err(BloomError::FusStatus {
                status,
                raw: xml_str.to_owned(),
            });
        }
    }

    let put = first_descendant(body, "Put").ok_or_else(|| BloomError::NoBinaryFile {
        model: model.to_owned(),
        region: region.to_owned(),
    })?;

    let size = first_data_by_tag(put, "BINARY_BYTE_SIZE")
        .filter(|s| !s.trim().is_empty())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .ok_or_else(|| BloomError::NoBinaryFile {
            model: model.to_owned(),
            region: region.to_owned(),
        })?;

    let file_name = first_data_by_tag(put, "BINARY_NAME")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| BloomError::NoBinaryFile {
            model: model.to_owned(),
            region: region.to_owned(),
        })?;

    let path = first_data_by_tag(put, "MODEL_PATH").ok_or_else(|| BloomError::NoBinaryFile {
        model: model.to_owned(),
        region: region.to_owned(),
    })?;

    let crc32 = first_data_by_tag(put, "BINARY_CRC")
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|v| v as u32);

    let v4_key = extract_v4_key(&doc);
    let fw_ver = first_data_by_tag(put, "BINARY_SW_VERSION");
    let model_type = first_data_by_tag(put, "DEVICE_MODEL_TYPE");
    let logic_val = first_data_by_tag(put, "LOGIC_VALUE_FACTORY")
        .or_else(|| first_data_by_tag(put, "LOGIC_VALUE_HOME"))
        .unwrap_or_default();

    let info = BinaryFileInfo {
        path,
        file_name,
        size,
        crc32,
        v4_key,
        fw_ver,
        model_type,
        logic_val,
    };

    if !allow_mismatch {
        let data_file = [
            "DEVICE_USER_DATA_FILE",
            "DEVICE_BOOT_FILE",
            "DEVICE_PDA_CODE1_FILE",
        ]
        .iter()
        .find_map(|tag| first_data_by_tag(put, tag).filter(|s| !s.trim().is_empty()));

        if let Some(f) = data_file {
            if let Some(data_index) = get_segment_index(Some(&f), model) {
                let fw_parts: Vec<&str> = fw.split('/').collect();
                if fw_parts.len() == 4 {
                    let (fw_version, fw_csc, fw_cp, fw_pda) =
                        (fw_parts[0], fw_parts[1], fw_parts[2], fw_parts[3]);
                    let fw_csc_suffix = fw_csc.split('_').nth(1);
                    let fw_cp_suffix = fw_cp.split('_').nth(1);

                    let split: Vec<&str> = f.split('_').collect();
                    if let Some(&version) = split.get(data_index) {
                        let version_suffix = split.get(data_index + 1).copied();

                        let csc_file = first_value_by_tag(put, "DEVICE_CSC_HOME_FILE")
                            .or_else(|| first_value_by_tag(put, "DEVICE_CSC_FILE"));
                        let csc_index = get_segment_index(csc_file.as_deref(), model);

                        let cp_file = first_value_by_tag(put, "DEVICE_PHONE_FONT_FILE");
                        let cp_index = get_segment_index(cp_file.as_deref(), model);

                        let pda_file = first_value_by_tag(put, "DEVICE_PDA_CODE1_FILE");
                        let pda_index = get_segment_index(pda_file.as_deref(), model);

                        let (served_csc, csc_suffix) = match (&csc_file, csc_index) {
                            (Some(cf), Some(ci)) => {
                                let parts: Vec<&str> = cf.split('_').collect();
                                (parts.get(ci).copied(), parts.get(ci + 1).copied())
                            }
                            _ => (None, None),
                        };

                        let (served_cp, cp_suffix) = match (&cp_file, cp_index) {
                            (Some(cf), Some(ci)) => {
                                let parts: Vec<&str> = cf.split('_').collect();
                                (parts.get(ci).copied(), parts.get(ci + 1).copied())
                            }
                            _ => (None, None),
                        };

                        let served_pda = match (&pda_file, pda_index) {
                            (Some(pf), Some(pi)) => pf.split('_').nth(pi),
                            _ => None,
                        };

                        let eff_csc = served_csc.or(version_suffix).unwrap_or("");
                        let eff_cp = served_cp.unwrap_or(version);
                        let eff_pda = served_pda.unwrap_or(version);

                        let served = format!("{version}/{eff_csc}/{eff_cp}/{eff_pda}");
                        let csc_match = fw_csc == eff_csc;
                        let cp_match = fw_cp == eff_cp;
                        let version_match = fw_version == version;
                        let pda_match = Some(fw_pda) == served_pda;
                        let csc_suffix_match = fw_csc_suffix.is_none_or(|s| Some(s) == csc_suffix);
                        let cp_suffix_match = fw_cp_suffix.is_none_or(|s| Some(s) == cp_suffix);

                        if served != fw
                            || !csc_match
                            || !cp_match
                            || !version_match
                            || !pda_match
                            || !csc_suffix_match
                            || !cp_suffix_match
                        {
                            return Err(BloomError::VersionMismatch {
                                requested: fw.to_owned(),
                                served,
                                info: Box::new(info),
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logic_check_matches_expected_indexing() {
        let input = "0123456789ABCDEF";
        let nonce = "\x00\x01\x0f\x10";
        assert_eq!(get_logic_check(input, nonce), "01F0");
    }

    #[test]
    fn version_xml_normalizes_three_part_version() {
        let xml = r#"<firmware><version><latest o="14(U)">S928BXXU1AXB5/S928BOXM1AXB5/S928BXXU1AXB5</latest></version></firmware>"#;
        let info = parse_version_xml(xml).unwrap();
        assert_eq!(
            info.version_code,
            "S928BXXU1AXB5/S928BOXM1AXB5/S928BXXU1AXB5/S928BXXU1AXB5"
        );
        assert_eq!(info.android_version, "14(U)");
    }

    #[test]
    fn binary_inform_xml_extracts_info_and_v4_key() {
        let xml = r#"
        <FUSMsg>
          <FUSBody>
            <Results>
              <Status>200</Status>
              <LATEST_FW_VERSION><Data>S928BXXU1AXB5/S928BOXM1AXB5/S928BXXU1AXB5/S928BXXU1AXB5</Data></LATEST_FW_VERSION>
            </Results>
            <Put>
              <BINARY_BYTE_SIZE><Data>17179869184</Data></BINARY_BYTE_SIZE>
              <BINARY_NAME><Data>SM-S928B_1_20240220120000_abcd1234_fac.zip.enc4</Data></BINARY_NAME>
              <MODEL_PATH><Data>SM-S928B/XID/</Data></MODEL_PATH>
              <BINARY_CRC><Data>3735928559</Data></BINARY_CRC>
              <BINARY_SW_VERSION><Data>S928BXXU1AXB5/S928BOXM1AXB5/S928BXXU1AXB5/S928BXXU1AXB5</Data></BINARY_SW_VERSION>
              <DEVICE_MODEL_TYPE><Data>1</Data></DEVICE_MODEL_TYPE>
              <LOGIC_VALUE_FACTORY><Data>0123456789ABCDEF</Data></LOGIC_VALUE_FACTORY>
            </Put>
          </FUSBody>
        </FUSMsg>"#;

        let info = parse_binary_inform(
            xml,
            "S928BXXU1AXB5/S928BOXM1AXB5/S928BXXU1AXB5/S928BXXU1AXB5",
            "SM-S928B",
            "XID",
            false,
        )
        .unwrap();

        assert_eq!(info.size, 17_179_869_184);
        assert_eq!(info.crc32, Some(0xDEAD_BEEF));
        assert!(info.v4_key.is_some());
    }

    #[test]
    fn binary_init_builds_expected_tags() {
        let xml = create_binary_init(
            "SM-S928B_1_20240220120000_abcd1234_fac.zip.enc4",
            "0123456789abcdef",
            Some("S928BXXU1AXB5/S928BOXM1AXB5/S928BXXU1AXB5/S928BXXU1AXB5"),
            Some("1"),
            "XID",
            false,
        );
        assert!(xml.contains("<BINARY_NAME><Data>SM-S928B_1_20240220120000_abcd1234_fac.zip.enc4</Data></BINARY_NAME>"));
        assert!(xml.contains("<DEVICE_LOCAL_CODE><Data>XID</Data></DEVICE_LOCAL_CODE>"));
        assert!(xml.contains("<LOGIC_CHECK><Data>"));
    }

    #[test]
    fn a_series_model_does_not_false_match_ap_prefix() {
        let xml = r#"
        <FUSMsg>
          <FUSBody>
            <Results>
              <Status>200</Status>
              <LATEST_FW_VERSION><Data>A075FXXU7DZJ5/A075FOLM7DZJ5/A075FXXU7DZJ5/A075FXXU7DZJ5</Data></LATEST_FW_VERSION>
            </Results>
            <Put>
              <BINARY_BYTE_SIZE><Data>9000000000</Data></BINARY_BYTE_SIZE>
              <BINARY_NAME><Data>SM-A075F_1_20261101120000_abcd1234_fac.zip.enc4</Data></BINARY_NAME>
              <MODEL_PATH><Data>SM-A075F/XID/</Data></MODEL_PATH>
              <DEVICE_PDA_CODE1_FILE><Data>AP_A075FXXU7DZJ5_QB12345_REV00_user_low_ship.tar.md5</Data></DEVICE_PDA_CODE1_FILE>
              <DEVICE_CSC_FILE><Data>CSC_OLM_A075FOLM7DZJ5_QB12345_REV00_user_low_ship.tar.md5</Data></DEVICE_CSC_FILE>
              <DEVICE_PHONE_FONT_FILE><Data>CP_A075FXXU7DZJ5_CP12345_REV00_user_low_ship.tar.md5</Data></DEVICE_PHONE_FONT_FILE>
              <LOGIC_VALUE_FACTORY><Data>0123456789ABCDEF</Data></LOGIC_VALUE_FACTORY>
            </Put>
          </FUSBody>
        </FUSMsg>"#;

        let info = parse_binary_inform(
            xml,
            "A075FXXU7DZJ5/A075FOLM7DZJ5/A075FXXU7DZJ5/A075FXXU7DZJ5",
            "SM-A075F",
            "XID",
            false,
        )
        .expect("SM-A075F should not confuse 'AP' with model suffix 'A'");

        assert_eq!(info.size, 9_000_000_000);
    }
}
