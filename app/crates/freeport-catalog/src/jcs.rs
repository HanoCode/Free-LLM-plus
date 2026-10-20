//! RFC 8785（JCS）规范化 JSON。
//!
//! 签名对象必须是**规范化的字节**：同一份内容用不同序列化器写出来，键顺序、
//! 空格、转义方式都可能不同，验签就会莫名其妙地失败。JCS 把这些差异钉死：
//!
//! - 对象键按 UTF-16 码元升序排列，键之间没有空格
//! - 字符串只转义 `"` `\` 与 U+0000–U+001F，其余字符（含中文）保持原样
//! - 数字不允许浮点：我们的签名载荷只有 `seq` 一个整数，
//!   而浮点数在 ECMAScript 里的格式化规则（最短往返表示）自己实现容易出偏差，
//!   与其「大致相等」，不如直接拒绝——签名载荷里出现浮点一定是设计出错。
//!
//! 这是一个**受限**实现，不是通用 JCS。限制写在上面三条里，并由单测钉住。

use serde_json::Value;

/// 把 `Value` 写成规范字节。出现浮点数直接报错。
pub fn canonical(v: &Value) -> Result<String, String> {
    let mut out = String::new();
    write_value(v, &mut out)?;
    Ok(out)
}

fn write_value(v: &Value, out: &mut String) -> Result<(), String> {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                out.push_str(&u.to_string());
            } else {
                return Err(format!("签名载荷不允许浮点数：{n}"));
            }
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            // RFC 8785：按 UTF-16 码元排序，不是按字节。ASCII 键两者一致，
            // 但带 emoji 或增补平面字符的键会不同，所以照规范来。
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_value(&map[*k], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorts_keys_and_strips_whitespace() {
        let v = json!({ "seq": 3, "channel": "stable", "version": "2026.10.19.1" });
        assert_eq!(
            canonical(&v).unwrap(),
            r#"{"channel":"stable","seq":3,"version":"2026.10.19.1"}"#
        );
    }

    #[test]
    fn same_content_different_key_order_gives_same_bytes() {
        let a: Value = serde_json::from_str(r#"{"a":1,"b":{"d":2,"c":3}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"b":{"c":3,"d":2},"a":1}"#).unwrap();
        assert_eq!(canonical(&a).unwrap(), canonical(&b).unwrap());
    }

    #[test]
    fn keeps_unicode_literal_and_escapes_controls() {
        let v = json!({ "t": "免费\t额度\u{1}" });
        assert_eq!(canonical(&v).unwrap(), "{\"t\":\"免费\\t额度\\u0001\"}");
    }

    #[test]
    fn rejects_floats() {
        let v = json!({ "cost": 0.5 });
        assert!(canonical(&v).unwrap_err().contains("浮点数"));
    }
}
