//! 按 JSON Pointer 精确改一个节点。
//!
//! 不用「整对象合并」：合并会顺手覆盖用户同层的其他字段，
//! 而这里的每一次写入都要能一字不差地还原。

use serde_json::{Map, Value};

/// 拆 `/a/b` → `["a", "b"]`。
fn segs(ptr: &str) -> Vec<&str> {
    ptr.split('/').filter(|s| !s.is_empty()).collect()
}

pub fn get<'a>(root: &'a Value, ptr: &str) -> Option<&'a Value> {
    let mut cur = root;
    for s in segs(ptr) {
        cur = cur.get(s)?;
    }
    Some(cur)
}

pub fn exists(root: &Value, ptr: &str) -> bool {
    get(root, ptr).is_some()
}

/// 写入并在有旧值时把旧值吐出来（用于回滚）。
pub fn set(root: &mut Value, ptr: &str, val: Value) -> Option<Value> {
    let parts = segs(ptr);
    if parts.is_empty() {
        return None;
    }
    let mut cur = root;
    for (i, s) in parts.iter().enumerate() {
        if i + 1 == parts.len() {
            let obj = ensure_object(cur);
            return obj.insert((*s).to_string(), val);
        }
        let obj = ensure_object(cur);
        cur = obj
            .entry((*s).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !cur.is_object() {
            // 中途不是对象（用户填了字符串），直接覆盖成对象：
            // 这种形状本来也无法安全合并
            *cur = Value::Object(Map::new());
        }
    }
    None
}

/// 删掉一个节点，并把因此变空的父对象一并清掉。
pub fn remove(root: &mut Value, ptr: &str) {
    let parts = segs(ptr);
    if parts.is_empty() {
        return;
    }
    remove_inner(root, &parts);
}

fn remove_inner(cur: &mut Value, parts: &[&str]) -> bool {
    if parts.len() == 1 {
        if let Some(obj) = cur.as_object_mut() {
            obj.remove(parts[0]);
            return obj.is_empty();
        }
        return false;
    }
    let empty = match cur.get_mut(parts[0]) {
        Some(child) => remove_inner(child, &parts[1..]),
        None => false,
    };
    if empty {
        if let Some(obj) = cur.as_object_mut() {
            obj.remove(parts[0]);
            return obj.is_empty();
        }
    }
    false
}

fn ensure_object(cur: &mut Value) -> &mut Map<String, Value> {
    if !cur.is_object() {
        *cur = Value::Object(Map::new());
    }
    cur.as_object_mut().expect("just made it an object")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn set_creates_missing_parents_and_returns_previous() {
        let mut doc = json!({ "permissions": { "allow": [] } });
        assert!(set(&mut doc, "/env/A", json!("x")).is_none());
        assert_eq!(doc["env"]["A"], "x");
        assert_eq!(set(&mut doc, "/env/A", json!("y")), Some(json!("x")));
        assert_eq!(doc["permissions"]["allow"], json!([]));
    }

    #[test]
    fn remove_prunes_empty_parents_only() {
        let mut doc = json!({ "env": { "A": "x" }, "keep": {} });
        remove(&mut doc, "/env/A");
        assert!(doc.get("env").is_none(), "空的 env 应当被清掉");
        assert!(doc.get("keep").is_some(), "本来就空的 keep 不归我们管");
    }

    #[test]
    fn overwriting_non_object_is_visible() {
        let mut doc = json!({ "provider": "legacy" });
        set(&mut doc, "/provider/freeport/x", json!(1));
        assert_eq!(doc["provider"]["freeport"]["x"], 1);
    }
}
