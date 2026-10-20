//! 注入的端到端验证。
//!
//! 全放在一个 `#[test]` 里：`FREEPORT_HOME` / `FREEPORT_DATA_DIR` 是进程级环境变量，
//! 拆成多个测试并行跑会互相串目录。

use std::path::Path;

use serde_json::{json, Value};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn injection_is_precise_idempotent_and_reversible() {
    let root = std::env::temp_dir().join(format!("fp-inj-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let data = root.join("data");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::env::set_var("FREEPORT_HOME", &home);
    std::env::set_var("FREEPORT_DATA_DIR", &data);

    let ctx = freeport_inject::Ctx::new("http://127.0.0.1:8787", "fp-0123456789abcdef", "auto");
    let supported = ["openai", "anthropic"];

    /* ── 1. Claude Code：JSON 定点写 + 字节级回滚 ── */
    let claude = home.join(".claude/settings.json");
    let original = "{\n  \"permissions\": { \"allow\": [\"Bash(npm:*)\"] },\n  \"includeCoAuthoredBy\": false\n}\n";
    write(&claude, original);

    let info = freeport_inject::list(&supported);
    let cc = info.iter().find(|a| a.id == "claude-code").unwrap();
    assert_eq!(cc.state, "detected");

    let preview = freeport_inject::preview("claude-code", &ctx).unwrap();
    let added: Vec<_> = preview.iter().filter(|l| l.kind == "add").collect();
    assert!(added.iter().any(|l| l.text.contains("ANTHROPIC_BASE_URL")));
    assert!(
        added
            .iter()
            .all(|l| !l.text.contains("fp-0123456789abcdef")),
        "预览里不能出现明文 key"
    );
    let masked = format!("{}…", &"fp-0123456789abcdef"[..8]);
    assert!(
        added.iter().any(|l| l.text.contains(&masked)),
        "预览应当显示掩码 {masked}"
    );

    let after = freeport_inject::apply("claude-code", &ctx, Some("key_local".into())).unwrap();
    assert_eq!(after.state, "injected");
    assert!(after.backup.is_some());

    let doc: Value = serde_json::from_str(&read(&claude)).unwrap();
    assert_eq!(doc["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8787");
    assert_eq!(doc["env"]["ANTHROPIC_AUTH_TOKEN"], "fp-0123456789abcdef");
    assert_eq!(doc["env"]["ANTHROPIC_MODEL"], "auto");
    assert_eq!(
        doc["permissions"]["allow"][0], "Bash(npm:*)",
        "别人的节点一个字都不能动"
    );

    // 幂等：再注入一次，文件字节不变
    let first = read(&claude);
    freeport_inject::apply("claude-code", &ctx, Some("key_local".into())).unwrap();
    assert_eq!(read(&claude), first, "重复注入必须幂等");

    // 回滚必须字节级还原
    let back = freeport_inject::revert("claude-code").unwrap();
    assert_eq!(back.state, "detected");
    assert_eq!(read(&claude), original, "回滚后应与原文完全一致");

    /* ── 2. Cline：覆盖别人的键，回滚要恢复旧值而不是删掉 ── */
    let cline = home.join(
        "Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_settings.json",
    );
    // 非 macOS 时用 spec 里的真实路径，避免测试绑定平台
    let cline = if cfg!(target_os = "macos") {
        cline
    } else {
        home.join(
            ".config/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_settings.json",
        )
    };
    let cline_original = json!({
        "apiProvider": "anthropic",
        "anthropicModelId": "claude-sonnet-4-5",
        "alwaysAllowReadOnly": true
    });
    write(
        &cline,
        &serde_json::to_string_pretty(&cline_original).unwrap(),
    );

    freeport_inject::apply("cline", &ctx, None).unwrap();
    let doc: Value = serde_json::from_str(&read(&cline)).unwrap();
    assert_eq!(doc["apiProvider"], "openai");
    assert_eq!(doc["openAiBaseUrl"], "http://127.0.0.1:8787/v1");
    assert_eq!(doc["alwaysAllowReadOnly"], true);

    freeport_inject::revert("cline").unwrap();
    let doc: Value = serde_json::from_str(&read(&cline)).unwrap();
    assert_eq!(doc["apiProvider"], "anthropic", "旧值必须被恢复");
    assert!(doc.get("openAiApiKey").is_none(), "我们加的键要删干净");
    assert_eq!(doc["anthropicModelId"], "claude-sonnet-4-5");

    /* ── 3. Codex：TOML 托管块插在第一个表头之前 ── */
    let codex = home.join(".codex/config.toml");
    let codex_original = "model = \"gpt-5-codex\"\napproval_policy = \"on-request\"\n\n[model_providers.other]\nname = \"x\"\n";
    write(&codex, codex_original);

    freeport_inject::apply("codex", &ctx, None).unwrap();
    let text = read(&codex);
    assert!(text.contains("# >>> freeport managed begin"));
    assert!(
        text.find("model_provider = \"freeport\"").unwrap()
            < text.find("[model_providers.other]").unwrap(),
        "根键必须在第一个表头之前"
    );
    let parsed: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(
        parsed["model_providers"]["other"]["name"].as_str(),
        Some("x")
    );

    let first = read(&codex);
    freeport_inject::apply("codex", &ctx, None).unwrap();
    assert_eq!(read(&codex), first, "重复注入不应叠加托管块");

    freeport_inject::revert("codex").unwrap();
    assert_eq!(read(&codex), codex_original, "摘块后应与原文一致");

    /* ── 5. JetBrains ACP：只寄宿已有条目，回滚字节级 ── */
    let acp = home.join(".jetbrains/acp.json");
    let acp_original = "{\n  \"agent_servers\": {\n    \"Gemini\": { \"command\": \"node\", \"args\": [\"gemini.js\"] },\n    \"Claude Code\": { \"command\": \"/usr/bin/node\", \"args\": [\"/opt/claude-agent-acp.js\"], \"env\": { \"KEEP\": \"1\" } }\n  }\n}\n";
    write(&acp, acp_original);

    let info = freeport_inject::list(&supported);
    let jb = info.iter().find(|a| a.id == "jetbrains").unwrap();
    assert_eq!(jb.discovery, "host");
    assert_eq!(jb.state, "detected");

    freeport_inject::apply("jetbrains", &ctx, None).unwrap();
    let doc: Value = serde_json::from_str(&read(&acp)).unwrap();
    let env = &doc["agent_servers"]["Claude Code"]["env"];
    assert_eq!(env["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8787");
    assert_eq!(env["KEEP"], "1", "条目自己的变量不许动");
    assert!(
        doc["agent_servers"]["Gemini"].get("env").is_none(),
        "别的 agent 一条都不该碰"
    );

    let first = read(&acp);
    freeport_inject::apply("jetbrains", &ctx, None).unwrap();
    assert_eq!(read(&acp), first, "重复注入必须幂等");

    freeport_inject::revert("jetbrains").unwrap();
    assert_eq!(read(&acp), acp_original, "回滚后应与原文完全一致");

    /* ── 6. Claude Desktop：三个文件一起写、一起还 ── */
    let desktop = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Claude-3p")
    } else if cfg!(target_os = "windows") {
        home.join("AppData/Local/Claude-3p")
    } else {
        home.join(".config/Claude-3p")
    };
    let lib_dir = desktop.join("configLibrary");
    let meta = lib_dir.join("_meta.json");
    let mode = desktop.join("claude_desktop_config.json");
    let def_id = "11111111-1111-5111-8111-111111111111";
    let meta_original = format!(
        "{{\n  \"entries\": [\n    {{ \"id\": \"{def_id}\", \"name\": \"Default\" }}\n  ],\n  \"appliedId\": \"{def_id}\"\n}}"
    );
    // MCP 配置和 deploymentMode 住同一个文件，回滚不能把用户的 MCP 弄丢
    let mode_original = "{\n  \"deploymentMode\": \"1p\",\n  \"mcpServers\": { \"fs\": { \"command\": \"npx\" } }\n}\n";
    write(&meta, &meta_original);
    write(&mode, mode_original);

    let info = freeport_inject::list(&supported);
    let cd = info.iter().find(|a| a.id == "claude-desktop").unwrap();
    assert_eq!(cd.state, "detected");

    // 预览要覆盖三个文件，且不能出现明文 key
    let preview = freeport_inject::preview("claude-desktop", &ctx).unwrap();
    let headers: Vec<_> = preview
        .iter()
        .filter(|l| l.kind == "ctx" && l.text.starts_with("── "))
        .collect();
    assert_eq!(headers.len(), 3, "三个文件都要在预览里露面");
    assert!(preview
        .iter()
        .all(|l| !l.text.contains("fp-0123456789abcdef")));

    freeport_inject::apply("claude-desktop", &ctx, Some("key_local".into())).unwrap();

    let profile: Value = serde_json::from_str(&read(
        &lib_dir.join("5f2c8a41-7d63-5b90-9e14-3c6a8b02d7f5.json"),
    ))
    .unwrap();
    assert_eq!(profile["inferenceProvider"], "gateway");
    assert_eq!(profile["inferenceGatewayBaseUrl"], "http://127.0.0.1:8787");
    assert_eq!(profile["inferenceGatewayApiKey"], "fp-0123456789abcdef");
    assert_eq!(profile["modelDiscoveryEnabled"], true);
    assert_eq!(
        profile["inferenceCustomHeaders"]["X-Freeport-Model-View"],
        "claude-desktop"
    );

    let m: Value = serde_json::from_str(&read(&meta)).unwrap();
    assert_eq!(m["appliedId"], "5f2c8a41-7d63-5b90-9e14-3c6a8b02d7f5");
    assert_eq!(
        m["entries"].as_array().unwrap().len(),
        2,
        "原来的 Default 档必须留着，否则用户回不去登录态"
    );

    let mode_doc: Value = serde_json::from_str(&read(&mode)).unwrap();
    assert_eq!(mode_doc["deploymentMode"], "3p");
    assert_eq!(
        mode_doc["mcpServers"]["fs"]["command"], "npx",
        "MCP 配置不许动"
    );

    let once = read(&meta);
    freeport_inject::apply("claude-desktop", &ctx, Some("key_local".into())).unwrap();
    assert_eq!(read(&meta), once, "重复注入必须幂等");

    freeport_inject::revert("claude-desktop").unwrap();
    assert_eq!(read(&meta), meta_original, "索引要字节级还原");
    assert_eq!(read(&mode), mode_original, "模式开关与 MCP 一起还原");
    assert!(
        !lib_dir
            .join("5f2c8a41-7d63-5b90-9e14-3c6a8b02d7f5.json")
            .exists(),
        "我们建的档要删掉，不能留孤儿"
    );

    /* ── 4. OpenCode：provider 子对象定点写，别人的 provider 不动 ── */
    let oc = home.join(".config/opencode/opencode.json");
    let oc_original = json!({
        "theme": "dark",
        "provider": { "other": { "name": "Other", "options": { "baseURL": "https://x.example/v1" } } }
    });
    write(&oc, &serde_json::to_string_pretty(&oc_original).unwrap());

    freeport_inject::apply("opencode", &ctx, None).unwrap();
    let doc: Value = serde_json::from_str(&read(&oc)).unwrap();
    assert_eq!(
        doc["provider"]["freeport"]["options"]["baseURL"],
        "http://127.0.0.1:8787/v1"
    );
    assert_eq!(
        doc["provider"]["freeport"]["options"]["apiKey"],
        "fp-0123456789abcdef"
    );
    assert_eq!(
        doc["provider"]["other"]["options"]["baseURL"], "https://x.example/v1",
        "别人的 provider 一个字都不能动"
    );
    let once = read(&oc);
    freeport_inject::apply("opencode", &ctx, None).unwrap();
    assert_eq!(read(&oc), once, "重复注入必须幂等");
    freeport_inject::revert("opencode").unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&read(&oc)).unwrap(),
        oc_original,
        "回滚后应与原文语义一致"
    );

    /* ── 5. Crush：只写 provider 与 discover_models，不写死模型表 ── */
    let crush = home.join(".config/crush/crush.json");
    let crush_original = json!({
        "theme": "dark",
        "providers": { "remote": { "type": "openai", "base_url": "https://x.example/v1" } }
    });
    write(&crush, &serde_json::to_string_pretty(&crush_original).unwrap());

    freeport_inject::apply("crush", &ctx, None).unwrap();
    let doc: Value = serde_json::from_str(&read(&crush)).unwrap();
    assert_eq!(doc["providers"]["freeport"]["type"], "openai-compat");
    assert_eq!(
        doc["providers"]["freeport"]["base_url"],
        "http://127.0.0.1:8787/v1"
    );
    assert_eq!(
        doc["providers"]["freeport"]["discover_models"], true,
        "模型表交给 Crush 自己拉，写死一周就过期"
    );
    assert!(
        doc["providers"]["freeport"].get("models").is_none(),
        "不该写死模型数组"
    );
    assert_eq!(doc["providers"]["remote"]["base_url"], "https://x.example/v1");
    freeport_inject::revert("crush").unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&read(&crush)).unwrap(),
        crush_original
    );

    /* ── 6. Claude Code VS Code 扩展：变量表追加，用户的代理变量留在原位 ── */
    let code_user = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Code/User")
    } else {
        home.join(".config/Code/User")
    };
    let vs = code_user.join("settings.json");
    let vs_original = json!({
        "editor.fontSize": 14,
        "claudeCode.environmentVariables": [
            { "name": "HTTP_PROXY", "value": "http://127.0.0.1:7890" }
        ]
    });
    write(&vs, &serde_json::to_string_pretty(&vs_original).unwrap());

    freeport_inject::apply("claude-code-vscode", &ctx, Some("key_vscode".into())).unwrap();
    let doc: Value = serde_json::from_str(&read(&vs)).unwrap();
    let vars = doc["claudeCode.environmentVariables"].as_array().unwrap();
    assert_eq!(
        vars[0]["name"], "HTTP_PROXY",
        "用户自己的变量必须留在原位"
    );
    assert!(
        vars.iter()
            .any(|v| v["name"] == "ANTHROPIC_BASE_URL" && v["value"] == "http://127.0.0.1:8787"),
        "网关地址要写进去：{vars:?}"
    );
    assert!(
        vars.iter().any(|v| v["name"] == "ANTHROPIC_AUTH_TOKEN"
            && v["value"] == "fp-0123456789abcdef"),
        "凭据要写进去"
    );
    assert_eq!(doc["claudeCode.disableLoginPrompt"], true);
    let once = read(&vs);
    freeport_inject::apply("claude-code-vscode", &ctx, Some("key_vscode".into())).unwrap();
    assert_eq!(read(&vs), once, "重复注入不能把变量追加两遍");
    freeport_inject::revert("claude-code-vscode").unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&read(&vs)).unwrap(),
        vs_original,
        "回滚要连 disableLoginPrompt 一起撤掉"
    );

    /* ── 7. VS Code Chat：写原生模型选择器的一组，其余组原样保留 ── */
    let chat = code_user.join("chatLanguageModels.json");
    let chat_original = json!([{
        "name": "Someone Else",
        "vendor": "openai",
        "apiKey": "sk-other",
        "models": [{ "id": "gpt-4o", "name": "GPT-4o", "maxInputTokens": 128000, "maxOutputTokens": 4096 }]
    }]);
    write(&chat, &serde_json::to_string_pretty(&chat_original).unwrap());

    let push_ctx = freeport_inject::Ctx::new(
        "http://127.0.0.1:8787",
        "fp-0123456789abcdef",
        "auto",
    )
    .with_models(vec![
        freeport_inject::ModelCard {
            id: "gpt-4.1".into(),
            label: "GPT-4.1".into(),
            context: 128_000,
            max_output: 16_384,
            tools: true,
            vision: true,
            reasoning: false,
        },
        freeport_inject::ModelCard {
            id: "claude-sonnet-4-5".into(),
            label: "Claude Sonnet 4.5".into(),
            context: 200_000,
            max_output: 32_000,
            tools: true,
            vision: true,
            reasoning: true,
        },
    ]);
    freeport_inject::apply("vscode-chat", &push_ctx, None).unwrap();
    let groups: Value = serde_json::from_str(&read(&chat)).unwrap();
    let arr = groups.as_array().unwrap();
    assert_eq!(arr.len(), 2, "应当只多出一组：{groups}");
    assert!(
        arr.iter().any(|g| g["apiKey"] == "sk-other"),
        "别人的组必须原样保留"
    );
    // 认领靠 `freeportVendor == "freeport"` 这个标记；`vendor` 是 VS Code 认识的
    // 供应商名，两者别混——标记值只有我们自己的常量 `push::MARK` 一个。
    let ours = arr
        .iter()
        .find(|g| g["freeportVendor"] == "freeport")
        .expect("找不到我们写的那组");
    assert_eq!(ours["name"], "Freeport");
    assert_eq!(ours["vendor"], "customendpoint");
    assert_eq!(ours["apiType"], "messages");
    assert!(
        ours.get("url").is_none(),
        "组级 url 会让 VS Code 转去自发现模型，模型表就白写了"
    );
    assert_eq!(ours["models"].as_array().unwrap().len(), 2);
    assert_eq!(ours["models"][0]["id"], "gpt-4.1");
    assert_eq!(
        ours["models"][0]["requestHeaders"]["x-api-key"],
        "fp-0123456789abcdef",
        "认证按条目走，组级 apiKey 会被当成 secret 引用"
    );
    assert_eq!(
        ours["models"][1]["defaultReasoningEffort"], "medium",
        "支持推理的模型才带推理档"
    );

    let once = read(&chat);
    freeport_inject::apply("vscode-chat", &push_ctx, None).unwrap();
    assert_eq!(read(&chat), once, "重复注入不能多出一组");

    freeport_inject::revert("vscode-chat").unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&read(&chat)).unwrap(),
        chat_original,
        "回滚后只剩别人的组"
    );

    /* ── 8. JetBrains ACP：只往已有条目注 env，不新建条目 ── */
    let acp = home.join(".jetbrains/acp.json");
    let acp_original = json!({
        "agent_servers": {
            "Claude Agent": {
                "command": "node",
                "args": ["/opt/claude-agent/acp.js"],
                "env": { "PATH": "/usr/local/bin" }
            },
            "Docs Agent": { "command": "python3", "args": ["docs.py"] }
        }
    });
    write(&acp, &serde_json::to_string_pretty(&acp_original).unwrap());

    freeport_inject::apply("jetbrains", &ctx, None).unwrap();
    let doc: Value = serde_json::from_str(&read(&acp)).unwrap();
    let env = &doc["agent_servers"]["Claude Agent"]["env"];
    assert_eq!(env["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8787");
    assert_eq!(env["FREEPORT_ACP"], "1", "认领标记要写进去，第二次靠它认人");
    assert_eq!(
        env["PATH"], "/usr/local/bin",
        "条目自己的 env 必须留着，否则 IDE 拉不起来"
    );
    assert_eq!(
        doc["agent_servers"]["Docs Agent"],
        acp_original["agent_servers"]["Docs Agent"],
        "别的 agent 条目一个字都不能动"
    );
    assert_eq!(
        doc["agent_servers"]["Claude Agent"]["command"], "node",
        "command / args 每台机器都不同，绝不改写"
    );

    let once = read(&acp);
    freeport_inject::apply("jetbrains", &ctx, None).unwrap();
    assert_eq!(read(&acp), once, "重复注入不能把 env 追加两遍");

    freeport_inject::revert("jetbrains").unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&read(&acp)).unwrap(),
        acp_original,
        "回滚要连 FREEPORT_ACP 一起撤掉"
    );

    /* ── 9. Claude Desktop：三个文件一起写、一起撤 ── */
    let d3p = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Claude-3p")
    } else {
        home.join(".config/Claude-3p")
    };
    // 用户已经有一套别的档，且当前正用着它：我们的写入不能把这一套弄丢
    let other_id = "11111111-2222-3333-4444-555555555555";
    let meta = d3p.join("configLibrary/_meta.json");
    let meta_original = json!({
        "entries": [{ "id": other_id, "name": "公司网关" }],
        "appliedId": other_id
    });
    write(&meta, &serde_json::to_string_pretty(&meta_original).unwrap());
    let mode = d3p.join("claude_desktop_config.json");
    let mode_original = json!({ "deploymentMode": "1p", "mcpServers": { "fs": { "command": "npx" } } });
    write(&mode, &serde_json::to_string_pretty(&mode_original).unwrap());

    freeport_inject::apply("claude-desktop", &ctx, None).unwrap();
    let m: Value = serde_json::from_str(&read(&meta)).unwrap();
    let entries = m["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "我们的档要追加，不能顶掉别人的");
    assert!(
        entries.iter().any(|e| e["id"] == other_id),
        "别人的档必须还在：{m}"
    );
    assert_eq!(m["appliedId"], "5f2c8a41-7d63-5b90-9e14-3c6a8b02d7f5");
    let profile = d3p.join("configLibrary/5f2c8a41-7d63-5b90-9e14-3c6a8b02d7f5.json");
    let p: Value = serde_json::from_str(&read(&profile)).unwrap();
    assert_eq!(p["inferenceProvider"], "gateway");
    assert_eq!(p["inferenceGatewayBaseUrl"], "http://127.0.0.1:8787");
    assert_eq!(p["inferenceGatewayApiKey"], "fp-0123456789abcdef");
    assert_eq!(p["modelDiscoveryEnabled"], true, "让 Desktop 自己拉模型表");
    assert_eq!(p["inferenceCustomHeaders"]["X-Freeport-Model-View"], "claude-desktop");
    let c: Value = serde_json::from_str(&read(&mode)).unwrap();
    assert_eq!(c["deploymentMode"], "3p");
    assert_eq!(c["mcpServers"]["fs"]["command"], "npx", "MCP 配置要留着");

    let trio = (read(&meta), read(&profile), read(&mode));
    freeport_inject::apply("claude-desktop", &ctx, None).unwrap();
    assert_eq!(
        (read(&meta), read(&profile), read(&mode)),
        trio,
        "重复注入不能把档登记两遍"
    );

    freeport_inject::revert("claude-desktop").unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&read(&meta)).unwrap(),
        meta_original,
        "索引要回到只有别人那一套"
    );
    assert_eq!(serde_json::from_str::<Value>(&read(&mode)).unwrap(), mode_original);
    assert!(!profile.exists(), "我们建的档要删掉，不能留垃圾在配置库里");

    /* ── 10. 未安装 / 只给步骤 / 协议不支持 ── */
    let info = freeport_inject::list(&supported);
    assert_eq!(
        info.iter().find(|a| a.id == "jetbrains").unwrap().state,
        "detected",
        "回滚之后文件还在，应当回到「未注入」"
    );

    // 拔掉 acp.json = 用户没装 Claude Agent。这时必须报未安装，
    // 而不是猜一个路径去写——猜错只会让 IDE 静默拉不起来。
    std::fs::remove_file(&acp).unwrap();
    let info = freeport_inject::list(&supported);
    assert_eq!(
        info.iter().find(|a| a.id == "jetbrains").unwrap().state,
        "missing"
    );
    let err = freeport_inject::apply("jetbrains", &ctx, None).unwrap_err();
    assert!(err.contains("没有检测到"), "报错要指出没装：{err}");

    // Guide 类永远报 guide：它本来就不写文件，装没装都只能给步骤
    let continue_ = info.iter().find(|a| a.id == "continue").unwrap();
    assert_eq!(continue_.state, "guide");
    assert!(continue_.path.is_none(), "Guide 不该给出一条假的落点路径");
    let err = freeport_inject::apply("continue", &ctx, None).unwrap_err();
    assert!(err.contains("手工"), "Guide 的执行要如实拒绝：{err}");

    let gemini = info.iter().find(|a| a.id == "gemini-cli").unwrap();
    assert_eq!(
        gemini.state, "unsupported",
        "网关没有 Gemini 协议时必须如实标注"
    );

    /* ── 11. 覆盖闸：会写文件的适配器必须每一个都真跑过 ── */
    let mut writable: Vec<&str> = info
        .iter()
        .filter(|a| a.strategy != "guide")
        .map(|a| a.id.as_str())
        .collect();
    writable.sort_unstable();
    let mut exercised = vec![
        "claude-code",
        "claude-code-vscode",
        "cline",
        "codex",
        "opencode",
        "crush",
        "vscode-chat",
        "jetbrains",
        "claude-desktop",
    ];
    exercised.sort_unstable();
    assert_eq!(
        writable, exercised,
        "有会写文件的适配器没进本测试；新增适配器必须同时补 e2e"
    );

    /* ── 12. 项目级 scope：只写仓库里那份个人文件，两个仓库互不干扰 ── */
    let repo_a = root.join("repo-a");
    let repo_b = root.join("repo-b");
    for r in [&repo_a, &repo_b] {
        std::fs::create_dir_all(r).unwrap();
    }
    // 共享的 settings.json 有同事的配置：项目级注入绝不能碰它
    let shared = repo_a.join(".claude/settings.json");
    let shared_original = "{\n  \"permissions\": { \"allow\": [\"Bash(make:*)\"] }\n}\n";
    write(&shared, shared_original);

    freeport_inject::apply_scoped(
        "claude-code",
        freeport_inject::Scope::Project(&repo_a),
        &ctx,
        None,
    )
    .unwrap();
    let local = repo_a.join(".claude/settings.local.json");
    let doc: Value = serde_json::from_str(&read(&local)).unwrap();
    assert_eq!(doc["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8787");
    assert_eq!(read(&shared), shared_original, "共享的 settings.json 一个字都不能动");

    // 用户级不能被这次写入污染：list 里仍应是「未注入」
    let after_project = freeport_inject::list(&supported);
    assert_eq!(
        after_project
            .iter()
            .find(|a| a.id == "claude-code")
            .unwrap()
            .state,
        "detected",
        "项目级注入不该让用户级显示成已注入"
    );
    let info = freeport_inject::project_info("claude-code", &repo_a).unwrap();
    assert_eq!(info.state, "injected");
    assert_eq!(info.path.as_deref(), Some(local.to_str().unwrap()));

    // 第二个仓库各注一次，回滚 A 不能把 B 也撤了
    freeport_inject::apply_scoped(
        "claude-code",
        freeport_inject::Scope::Project(&repo_b),
        &ctx,
        None,
    )
    .unwrap();
    assert_eq!(freeport_inject::project_list(&repo_a).len(), 1);
    assert_eq!(freeport_inject::project_list(&repo_b).len(), 1);
    freeport_inject::revert_scoped("claude-code", freeport_inject::Scope::Project(&repo_a)).unwrap();
    assert!(!local.exists(), "注入前没有这个文件，回滚就删掉它");
    assert_eq!(read(&shared), shared_original);
    assert!(
        repo_b.join(".claude/settings.local.json").exists(),
        "回滚 A 不能连带撤掉 B"
    );
    assert!(
        freeport_inject::project_list(&repo_a).is_empty(),
        "日志里不该留下 A 的残留"
    );

    // 幂等：重复注入同一仓库，字节一致
    let once = read(&repo_b.join(".claude/settings.local.json"));
    freeport_inject::apply_scoped(
        "claude-code",
        freeport_inject::Scope::Project(&repo_b),
        &ctx,
        None,
    )
    .unwrap();
    assert_eq!(read(&repo_b.join(".claude/settings.local.json")), once);

    // 没有个人文件的适配器必须拒绝项目级，而不是往仓库里写 key
    for id in ["opencode", "crush", "codex", "vscode-chat"] {
        let err = freeport_inject::apply_scoped(
            id,
            freeport_inject::Scope::Project(&repo_b),
            &ctx,
            None,
        )
        .unwrap_err();
        assert!(
            err.contains("项目级"),
            "{id} 的项目级应当被拒绝并说明原因：{err}"
        );
    }
    let err = freeport_inject::apply_scoped(
        "claude-desktop",
        freeport_inject::Scope::Project(&repo_b),
        &ctx,
        None,
    )
    .unwrap_err();
    assert!(err.contains("用户级"), "{err}");
    // 路径写成文件而不是目录，立刻报错，不要造出一棵没人读的目录树
    let err = freeport_inject::apply_scoped(
        "claude-code",
        freeport_inject::Scope::Project(&shared),
        &ctx,
        None,
    )
    .unwrap_err();
    assert!(err.contains("项目目录不存在"), "{err}");

    let _ = std::fs::remove_dir_all(&root);
}
