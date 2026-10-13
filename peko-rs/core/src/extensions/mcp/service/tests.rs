use super::*;
use peko_engine::ToolFunnel;

#[cfg(unix)]
struct Fixture {
    _dir: tempfile::TempDir,
    runtime: Arc<PrincipalMcpRuntime>,
    manifest: PathBuf,
    root: PathBuf,
}

#[cfg(unix)]
impl Fixture {
    fn new(owner: &str, vault: Arc<crate::common::vault::Vault>, label: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace/mcp/shared");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("server.py"), r#"
import sys,json,os,hashlib,time
with open('starts.txt','a') as f: f.write('start\n')
for line in sys.stdin:
    request=json.loads(line)
    method=request.get('method'); params=request.get('params',{})
    if 'id' not in request: continue
    error=None
    if method=='initialize':
        if os.path.exists('offline'): os._exit(0)
        result={'protocolVersion':'2025-03-26','capabilities':{'tools':{},'resources':{},'prompts':{}},'serverInfo':{'name':'fixture','version':'1'},'instructions':'Use the echo method.'}
    elif method=='tools/list':
        tool=json.load(open('schema.json'))
        if os.path.exists('repeat_cursor'): result={'tools':[tool],'nextCursor':'again'}
        else: result={'tools':[tool]}
    elif method=='resources/list': result={'resources':[{'uri':'fixture://resource','name':'fixture'}]}
    elif method=='resources/templates/list': result={'resourceTemplates':[{'uriTemplate':'fixture://{name}','name':'fixture'}]}
    elif method=='resources/read': result={'contents':[{'uri':params['uri'],'text':'resource contents'}]}
    elif method=='prompts/list': result={'prompts':[{'name':'review','description':'Review material'}]}
    elif method=='prompts/get': result={'messages':[{'role':'user','content':{'type':'text','text':'prompt contents'}}]}
    elif method=='tools/call':
        args=params.get('arguments',{})
        with open('calls.jsonl','a') as f: f.write(json.dumps(args)+'\n')
        if args.get('value')=='exit': os._exit(0)
        if args.get('value')=='notify':
            tool=json.load(open('schema.json'));tool['description']='Changed by notification';json.dump(tool,open('schema.json','w'))
            print(json.dumps({'jsonrpc':'2.0','method':'notifications/tools/list_changed'}),flush=True)
        if args.get('value')=='slow': time.sleep(0.5)
        value={'method':params['name'],'label':os.environ['LABEL'],'principal':args.get('principal_id'),'session':args.get('session_id'),'run':args.get('run_id'),'call':args.get('tool_call_id'),'token_digest':hashlib.sha256(os.environ['TOKEN'].encode()).hexdigest(),'value':args.get('value')}
        if args.get('value')=='sample':
            print(json.dumps({'jsonrpc':'2.0','id':'sampling-1','method':'sampling/createMessage','params':{'messages':[{'role':'user','content':{'type':'text','text':'Say hi'}}],'maxTokens':64}}),flush=True)
            sampling=json.loads(sys.stdin.readline())
            value['sampling']=sampling.get('result') or sampling.get('error')
        result={'content':[{'type':'text','text':json.dumps(value)}],'structuredContent':value,'isError':args.get('value')=='fail'}
    elif method=='ping': result={}
    else: error={'code':-32601,'message':'unsupported method'}
    response={'jsonrpc':'2.0','id':request['id']}
    response['error' if error else 'result']=error or result
    print(json.dumps(response),flush=True)
"#).unwrap();
        std::fs::write(root.join("schema.json"),json!({"name":"echo","description":"Echo the value","inputSchema":{"type":"object","properties":{"value":{"type":"string"},"principal_id":{"type":"string"},"session_id":{"type":"string"},"run_id":{"type":"string"},"tool_call_id":{"type":"string"}},"required":["value","principal_id","session_id","run_id","tool_call_id"],"additionalProperties":false}}).to_string()).unwrap();
        let manifest = root.join("server.json");
        std::fs::write(&manifest,json!({"schema_version":1,"transport":{"type":"stdio","command":which::which("python3").unwrap(),"args":["server.py"]},"env":{"LABEL":{"value":label},"TOKEN":{"credential":"api_token"}},"health_check_interval_secs":0,"reserved_parameters":{"principal_id":{"source":"runtime","field":"principal_id"},"session_id":{"source":"runtime","field":"session_id"},"run_id":{"source":"runtime","field":"run_id"},"tool_call_id":{"source":"runtime","field":"tool_call_id"}}}).to_string()).unwrap();
        let runtime = PrincipalMcpRuntime::new(
            PrincipalId(owner.into()),
            dir.path().join("workspace"),
            dir.path().join("local/cache"),
            Some(vault.clone()),
            None,
            Arc::new(peko_quota::QuotaMeter::unlimited()),
            None,
        );
        vault
            .set_credential(&crate::common::vault::Credential::now(
                runtime.namespace("shared"),
                "api_token",
                crate::common::vault::CredentialKind::ApiKey,
                secrecy::SecretString::new(format!("secret-{label}").into()),
            ))
            .unwrap();
        Self {
            _dir: dir,
            runtime,
            manifest,
            root,
        }
    }
    fn context(&self, session: &str) -> ToolContext {
        ToolContext::for_hook_run("run", "call", "echo")
            .with_principal_id(&self.runtime.owner.0)
            .with_session_id(session)
    }
    fn call(
        &self,
        session: &str,
        value: &str,
        binding: Option<Arc<dyn peko_engine::tooling::ToolCatalogBinding>>,
    ) -> peko_engine::ToolCallSpec {
        let mut call = peko_engine::ToolCallSpec::new("mcp__shared__echo", json!({"value":value}));
        call.run_id = Some("engine-run".into());
        call.tool_call_id = Some("model-tool-call".into());
        call.principal_id = Some(self.runtime.owner.0.clone());
        call.session_id = Some(session.into());
        call.catalog_binding = binding;
        call
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cold_discovery_isolated_owners_and_shared_sibling_startup() {
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(crate::common::vault::Vault::for_test(
        vault_dir.path(),
        "mcp-test-passphrase",
    ));
    let first = Fixture::new("first", vault.clone(), "A");
    let second = Fixture::new("second", vault, "B");
    let tooling = crate::tools::runtime::ToolingRuntime::standalone();
    tooling.bind_mcp(first.runtime.clone()).await;
    tooling.bind_mcp(second.runtime.clone()).await;
    let (a, b) = tokio::join!(
        tooling.list_tool_definitions(first.runtime.owner()),
        tooling.list_tool_definitions(first.runtime.owner())
    );
    assert!(a
        .definitions
        .iter()
        .any(|tool| tool.name == "mcp__shared__echo"));
    assert_eq!(
        serde_json::to_value(&a.definitions).unwrap(),
        serde_json::to_value(&b.definitions).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(first.root.join("starts.txt"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let c = tooling.list_tool_definitions(second.runtime.owner()).await;
    for (fixture, snapshot, label, session) in [
        (&first, &a, "A", "sibling-1"),
        (&first, &b, "A", "sibling-2"),
        (&second, &c, "B", "sibling-1"),
    ] {
        let (text, result, success) = tooling
            .execute(fixture.call(session, "hello", snapshot.binding.clone()))
            .await
            .unwrap();
        assert!(success, "{text}");
        assert_eq!(result["structuredContent"]["run"], "engine-run");
        assert_eq!(result["structuredContent"]["call"], "model-tool-call");
        assert_eq!(result["structuredContent"]["label"], label);
        assert_eq!(result["structuredContent"]["session"], session);
        assert_eq!(
            result["structuredContent"]["principal"],
            fixture.runtime.owner.0
        );
        assert_eq!(
            result["structuredContent"]["token_digest"],
            hex::encode(Sha256::digest(format!("secret-{label}").as_bytes()))
        );
    }
    let mut foreign = second.call("foreign", "hello", a.binding.clone());
    foreign.principal_id = Some(second.runtime.owner.0.clone());
    assert!(!tooling.execute(foreign).await.unwrap().2);
    let tool = first.runtime.tools().await.pop().unwrap().1;
    assert!(tool
        .execute_with_context(json!({"value":"hello"}), &second.context("foreign"))
        .await
        .unwrap_err()
        .to_string()
        .contains("does not own"));
    first.runtime.shutdown().await;
    second.runtime.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn schema_refresh_removal_and_started_call_leases() {
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(crate::common::vault::Vault::for_test(
        vault_dir.path(),
        "mcp-test-passphrase",
    ));
    let fixture = Fixture::new("owner", vault, "A");
    let tooling = crate::tools::runtime::ToolingRuntime::standalone();
    tooling.bind_mcp(fixture.runtime.clone()).await;
    let snapshot = tooling.list_tool_definitions(fixture.runtime.owner()).await;
    let mut schema: Value =
        serde_json::from_slice(&std::fs::read(fixture.root.join("schema.json")).unwrap()).unwrap();
    schema["description"] = json!("A changed tool description");
    std::fs::write(fixture.root.join("schema.json"), schema.to_string()).unwrap();
    fixture
        .runtime
        .operate(
            "refresh",
            Some("shared"),
            &json!({}),
            &fixture.context("session"),
        )
        .await
        .unwrap();
    let stale = tooling
        .execute(fixture.call("session", "hello", snapshot.binding.clone()))
        .await
        .unwrap();
    assert!(!stale.2);
    assert!(stale.0.contains("McpCatalogChanged"));
    assert!(!fixture.root.join("calls.jsonl").exists());
    let snapshot = tooling.list_tool_definitions(fixture.runtime.owner()).await;
    let background = {
        let tooling = tooling.clone();
        let call = fixture.call("session", "slow", snapshot.binding.clone());
        tokio::spawn(async move { tooling.execute(call).await.unwrap() })
    };
    for _ in 0..100 {
        if fixture.root.join("calls.jsonl").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(fixture.root.join("calls.jsonl").exists());
    std::fs::remove_file(&fixture.manifest).unwrap();
    let updated = tooling.list_tool_definitions(fixture.runtime.owner()).await;
    assert!(!updated
        .definitions
        .iter()
        .any(|tool| tool.name.starts_with("mcp__")));
    assert!(background.await.unwrap().2);
    let removed = tooling
        .execute(fixture.call("session", "hello", snapshot.binding))
        .await
        .unwrap();
    assert!(!removed.2);
    assert!(removed.0.contains("McpCatalogChanged") || removed.0.contains("McpServerRemoved"));
    fixture.runtime.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn cached_catalog_starts_lazily_and_protocol_errors_remain_structured() {
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(crate::common::vault::Vault::for_test(
        vault_dir.path(),
        "mcp-test-passphrase",
    ));
    let fixture = Fixture::new("owner", vault.clone(), "A");
    fixture.runtime.prepare_catalog().await.unwrap();
    let cached = PrincipalMcpRuntime::new(
        fixture.runtime.owner.clone(),
        fixture.runtime.workspace.clone(),
        fixture.runtime.cache_dir.clone(),
        Some(vault),
        None,
        fixture.runtime.meter.clone(),
        None,
    );
    let tooling = crate::tools::runtime::ToolingRuntime::standalone();
    tooling.bind_mcp(cached.clone()).await;
    let snapshot = tooling.list_tool_definitions(cached.owner()).await;
    assert_eq!(cached.status().await["servers"][0]["catalog"], "cached");
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("starts.txt"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let (_, result, success) = tooling
        .execute(fixture.call("session", "fail", snapshot.binding))
        .await
        .unwrap();
    assert!(!success);
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["value"], "fail");
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("starts.txt"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    cached.shutdown().await;
    fixture.runtime.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn resource_prompt_access_and_repeated_cursor_failures() {
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(crate::common::vault::Vault::for_test(
        vault_dir.path(),
        "mcp-test-passphrase",
    ));
    let fixture = Fixture::new("owner", vault, "A");
    fixture.runtime.prepare_catalog().await.unwrap();
    let context = fixture.context("session");
    assert_eq!(
        fixture
            .runtime
            .operate(
                "read_resource",
                Some("shared"),
                &json!({"uri":"fixture://resource"}),
                &context
            )
            .await
            .unwrap()["contents"][0]["text"],
        "resource contents"
    );
    assert_eq!(
        fixture
            .runtime
            .operate(
                "get_prompt",
                Some("shared"),
                &json!({"name":"review"}),
                &context
            )
            .await
            .unwrap()["messages"][0]["content"]["text"],
        "prompt contents"
    );
    assert_eq!(
        fixture
            .runtime
            .operate("resource_templates", Some("shared"), &json!({}), &context)
            .await
            .unwrap()["resourceTemplates"][0]["name"],
        "fixture"
    );
    std::fs::write(fixture.root.join("repeat_cursor"), "").unwrap();
    let error = fixture
        .runtime
        .operate("refresh", Some("shared"), &json!({}), &context)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("pagination cursor"));
    fixture.runtime.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn sdk_sampling_uses_the_explicit_owner_model_and_meter() {
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(crate::common::vault::Vault::for_test(
        vault_dir.path(),
        "sampling-test-passphrase",
    ));
    let mut fixture = Fixture::new("sampling-owner", vault.clone(), "A");
    let mut definition: Value =
        serde_json::from_slice(&std::fs::read(&fixture.manifest).unwrap()).unwrap();
    definition["sampling"] = json!({"max_output_tokens":128});
    std::fs::write(&fixture.manifest, definition.to_string()).unwrap();
    let adapter = peko_providers::MockAdapter::new();
    adapter.queue_text("owner-model response");
    let (resolver, _) =
        peko_providers::LlmResolver::mock(adapter, &vault_dir.path().join("models.toml")).await;
    let model = resolver.catalog().list_enabled().await[0].id.clone();
    let meter = Arc::new(
        peko_quota::QuotaMeter::load_or_init(
            peko_quota::QuotaConfig {
                input_tokens: Some(1_000_000),
                ..Default::default()
            },
            None,
            chrono::Utc::now(),
        )
        .await
        .unwrap(),
    );
    fixture.runtime = PrincipalMcpRuntime::new(
        fixture.runtime.owner.clone(),
        fixture.runtime.workspace.clone(),
        fixture.runtime.cache_dir.clone(),
        Some(vault),
        Some(resolver),
        meter.clone(),
        Some(model),
    );
    fixture.runtime.prepare_catalog().await.unwrap();
    let tool = fixture.runtime.tools().await.pop().unwrap().1;
    let before = meter.snapshot();
    let result = tool
        .execute_with_context(
            json!({"value":"sample"}),
            &fixture.context("sampling-session"),
        )
        .await
        .unwrap();
    assert_eq!(
        result["structuredContent"]["sampling"]["content"]["text"],
        "owner-model response"
    );
    assert!(meter.snapshot().output_tokens > before.output_tokens);
    fixture.runtime.set_model(None);
    let used = meter.snapshot().output_tokens;
    let result = tool
        .execute_with_context(
            json!({"value":"sample"}),
            &fixture.context("sampling-session"),
        )
        .await
        .unwrap();
    assert!(result["structuredContent"]["sampling"]["message"]
        .as_str()
        .unwrap()
        .contains("No principal model"));
    assert_eq!(meter.snapshot().output_tokens, used);
    fixture.runtime.shutdown().await;
}

#[test]
fn configuration_preserves_fields_and_rejects_unknowns() {
    let path = std::path::Path::new("/tmp/workspace/mcp/sample/server.json");
    let value = json!({"schema_version":1,"transport":{"type":"stdio","command":"python3","args":["server.py"]},"env":{"TOKEN":{"credential":"api_token"}},"cwd":"subdir","tool_timeout_ms":null,"reserved_parameters":{"session_id":{"source":"runtime","field":"session_id"}}});
    let definition = Definition::parse(value.clone(), path, "sample").unwrap();
    assert_eq!(
        definition.cwd.as_deref(),
        Some(std::path::Path::new("/tmp/workspace/mcp/sample/subdir"))
    );
    assert!(definition.tool_timeout_ms.is_none());
    assert_eq!(definition.reserved_parameters.len(), 1);
    let mut invalid = value;
    invalid["unknown"] = json!(true);
    assert!(Definition::parse(invalid, path, "sample").is_err());
}

#[tokio::test]
async fn manifests_are_owned_and_duplicate_definitions_fail() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("mcp/sample");
    tokio::fs::create_dir_all(&root).await.unwrap();
    let config = json!({"schema_version":1,"transport":{"type":"stdio","command":"unused"}});
    tokio::fs::write(root.join("server.json"), config.to_string())
        .await
        .unwrap();
    let runtime = PrincipalMcpRuntime::new(
        PrincipalId("owner".into()),
        directory.path().into(),
        directory.path().join("cache"),
        None,
        None,
        Arc::new(peko_quota::QuotaMeter::unlimited()),
        None,
    );
    runtime.reconcile().await.unwrap();
    assert_eq!(runtime.status().await["servers"][0]["server"], "sample");
    tokio::fs::write(root.join("manifest.yaml"), "mcp_servers: {}")
        .await
        .unwrap();
    runtime.reconcile().await.unwrap();
    assert_eq!(
        runtime.status().await["servers"][0]["state"],
        "invalid_config"
    );
    assert!(runtime.tools().await.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn owner_teardown_cancels_calls_on_retired_connections() {
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(crate::common::vault::Vault::for_test(
        vault_dir.path(),
        "teardown-passphrase",
    ));
    let fixture = Fixture::new("owner", vault, "A");
    fixture.runtime.prepare_catalog().await.unwrap();
    let tool = fixture.runtime.tools().await.pop().unwrap().1;
    let context = fixture.context("session");
    let call = tokio::spawn(async move {
        tool.execute_with_context(json!({"value":"slow"}), &context)
            .await
    });
    for _ in 0..100 {
        if fixture.root.join("calls.jsonl").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(fixture.root.join("calls.jsonl").exists());
    std::fs::remove_file(&fixture.manifest).unwrap();
    fixture.runtime.reconcile().await.unwrap();
    fixture.runtime.shutdown().await;
    assert!(call.await.unwrap().is_err());
}

#[tokio::test]
async fn legacy_json_and_yaml_preserve_the_same_complete_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mcp/shared");
    tokio::fs::create_dir_all(&path).await.unwrap();
    let definition = json!({"transport":{"type":"sse","endpoint":"https://example.com/sse"},"headers":{"Accept-Language":"en"},"auth":{"type":"bearer","credential":"token"},"auto_start":false,"init_timeout_secs":5,"tool_timeout_secs":10,"reserved_parameters":{"principal_id":{"source":"runtime","field":"principal_id"}}});
    tokio::fs::write(path.join("server.json"), definition.to_string())
        .await
        .unwrap();
    let json_config = config::scan(&directory.path().join("mcp"))
        .await
        .unwrap()
        .remove("shared")
        .unwrap()
        .definition
        .unwrap();
    tokio::fs::remove_file(path.join("server.json"))
        .await
        .unwrap();
    tokio::fs::write(
        path.join("manifest.yaml"),
        serde_yaml::to_string(&json!({"mcp_servers":{"shared":definition}})).unwrap(),
    )
    .await
    .unwrap();
    let yaml_config = config::scan(&directory.path().join("mcp"))
        .await
        .unwrap()
        .remove("shared")
        .unwrap()
        .definition
        .unwrap();
    assert_eq!(
        serde_json::to_value(&json_config).unwrap(),
        serde_json::to_value(&yaml_config).unwrap()
    );
    assert_eq!(json_config.startup_timeout_ms, 5000);
    assert_eq!(json_config.tool_timeout_ms, Some(10000));
    assert_eq!(json_config.reserved_parameters.len(), 1);
    assert!(matches!(json_config.auth, Authentication::Bearer { .. }));
}

#[cfg(unix)]
#[tokio::test]
async fn sdk_notifications_refresh_catalog_and_disconnect_does_not_replay_business_calls() {
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(crate::common::vault::Vault::for_test(
        vault_dir.path(),
        "notification-passphrase",
    ));
    let fixture = Fixture::new("owner", vault, "A");
    let tooling = crate::tools::runtime::ToolingRuntime::standalone();
    tooling.bind_mcp(fixture.runtime.clone()).await;
    let initial = tooling.list_tool_definitions(fixture.runtime.owner()).await;
    assert!(
        tooling
            .execute(fixture.call("session", "notify", initial.binding.clone()))
            .await
            .unwrap()
            .2
    );
    let updated = tooling.list_tool_definitions(fixture.runtime.owner()).await;
    assert_eq!(
        updated
            .definitions
            .iter()
            .find(|tool| tool.name == "mcp__shared__echo")
            .unwrap()
            .description,
        "Changed by notification"
    );
    assert!(
        !tooling
            .execute(fixture.call("session", "hello", initial.binding))
            .await
            .unwrap()
            .2
    );
    let outcome = tooling
        .execute(fixture.call("session", "exit", updated.binding))
        .await
        .unwrap();
    assert!(!outcome.2);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("calls.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    fixture.runtime.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_one_request_keeps_the_shared_connection_usable() {
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(crate::common::vault::Vault::for_test(
        vault_dir.path(),
        "cancel-passphrase",
    ));
    let fixture = Fixture::new("owner", vault, "A");
    fixture.runtime.prepare_catalog().await.unwrap();
    let tool = fixture.runtime.tools().await.pop().unwrap().1;
    let (abort, receiver) = tokio::sync::watch::channel(false);
    let context = ToolContext::for_hook_run_with_abort("run", "call", "echo", receiver)
        .with_principal_id("owner")
        .with_session_id("first");
    let first = tool.clone();
    let request = tokio::spawn(async move {
        first
            .execute_with_context(json!({"value":"slow"}), &context)
            .await
    });
    for _ in 0..100 {
        if fixture.root.join("calls.jsonl").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(fixture.root.join("calls.jsonl").exists());
    abort.send(true).unwrap();
    assert!(request.await.unwrap().is_err());
    let result = tool
        .execute_with_context(json!({"value":"hello"}), &fixture.context("second"))
        .await
        .unwrap();
    assert_eq!(result["structuredContent"]["session"], "second");
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("starts.txt"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    fixture.runtime.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn sibling_retries_share_one_startup_even_at_the_restart_limit() {
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(crate::common::vault::Vault::for_test(
        vault_dir.path(),
        "retry-passphrase",
    ));
    let fixture = Fixture::new("owner", vault, "A");
    let mut definition: Value =
        serde_json::from_slice(&std::fs::read(&fixture.manifest).unwrap()).unwrap();
    definition["max_restarts"] = json!(1);
    std::fs::write(&fixture.manifest, definition.to_string()).unwrap();
    std::fs::write(fixture.root.join("offline"), "").unwrap();
    fixture.runtime.prepare_catalog().await.unwrap();
    assert!(fixture.runtime.tools().await.is_empty());
    let server = fixture.runtime.server("shared").await.unwrap();
    std::fs::remove_file(fixture.root.join("offline")).unwrap();
    server.retry_after.store(0, Ordering::Release);
    let (first, second) = tokio::join!(
        fixture.runtime.start(&server),
        fixture.runtime.start(&server)
    );
    assert!(Arc::ptr_eq(&first.unwrap(), &second.unwrap()));
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("starts.txt"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    fixture.runtime.shutdown().await;
}

#[test]
fn registered_names_fit_model_apis_without_losing_remote_identity() {
    use super::proxy::registered_name;
    assert_eq!(registered_name("shared", "echo"), "mcp__shared__echo");
    let pairs = [
        ("github.test".to_owned(), "search.items".to_owned()),
        ("github_test".into(), "search_items".into()),
        ("a__b".into(), "c".into()),
        ("a".into(), "b__c".into()),
        ("server".into(), "query_".repeat(40)),
        ("server".into(), "query_".repeat(41)),
        ("server".into(), "search.雪".into()),
    ];
    let mut names = std::collections::HashSet::new();
    for (server, method) in pairs {
        let name = registered_name(&server, &method);
        assert!(
            name.len() <= 64
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        );
        assert_eq!(name, registered_name(&server, &method));
        assert!(names.insert(name));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn model_aliases_invoke_exact_remote_methods_and_duplicate_aliases_fail_closed() {
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(crate::common::vault::Vault::for_test(
        vault_dir.path(),
        "name-passphrase",
    ));
    let fixture = Fixture::new("owner", vault, "A");
    let schema_path = fixture.root.join("schema.json");
    let mut descriptor: Value =
        serde_json::from_slice(&std::fs::read(&schema_path).unwrap()).unwrap();
    let remote_name = "search.items";
    descriptor["name"] = json!(remote_name);
    std::fs::write(schema_path, descriptor.to_string()).unwrap();
    let tooling = crate::tools::runtime::ToolingRuntime::standalone();
    tooling.bind_mcp(fixture.runtime.clone()).await;
    let snapshot = tooling.list_tool_definitions(fixture.runtime.owner()).await;
    let registered = super::proxy::registered_name("shared", remote_name);
    assert!(snapshot
        .definitions
        .iter()
        .any(|tool| tool.name == registered));
    let mut call = fixture.call("session", "hello", snapshot.binding);
    call.tool_name = registered;
    let (text, value, success) = tooling.execute(call).await.unwrap();
    assert!(success, "{text}");
    assert_eq!(value["structuredContent"]["method"], remote_name);
    let server = fixture.runtime.server("shared").await.unwrap();
    server
        .inventory
        .write()
        .await
        .as_mut()
        .unwrap()
        .tools
        .push(descriptor);
    assert!(fixture.runtime.tools().await.is_empty());
    assert!(fixture.runtime.status().await["servers"][0]["error"]
        .as_str()
        .unwrap()
        .contains("name collision"));
    fixture.runtime.shutdown().await;
}
