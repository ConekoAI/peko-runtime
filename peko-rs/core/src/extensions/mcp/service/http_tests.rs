//! Actual HTTP transport and consent fixtures, with no external accounts.
#![cfg(unix)]
use super::*;
use peko_engine::ToolFunnel;
use tokio::io::{AsyncBufReadExt, BufReader};

struct HttpFixture {
    directory: tempfile::TempDir,
    _child: tokio::process::Child,
    url: String,
}
impl HttpFixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("fixture_http_server.py");
        std::fs::write(&script,r#"
import json,urllib.parse,hashlib,base64,queue
from http.server import ThreadingHTTPServer,BaseHTTPRequestHandler
class Handler(BaseHTTPRequestHandler):
    def log_message(self,*args): pass
    def reply(self,status,value=None,headers={}):
        body=json.dumps(value).encode() if value is not None else b''
        self.send_response(status)
        self.send_header('Content-Type','application/json')
        self.send_header('Content-Length',str(len(body)))
        for key,value in headers.items(): self.send_header(key,value)
        self.end_headers();self.wfile.write(body)
    def do_GET(self):
        base='http://127.0.0.1:'+str(self.server.server_port)
        path=urllib.parse.urlsplit(self.path)
        if path.path=='/sse':
            self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
            self.wfile.write(b'event: endpoint\r\ndata: /messages?session=fixture\r\n\r\n');self.wfile.flush()
            while True:
                value=self.server.messages.get()
                payload=('event: message\r\ndata: '+json.dumps(value)+'\r\n\r\n').encode()
                self.wfile.write(payload);self.wfile.flush()
        elif path.path=='/.well-known/oauth-protected-resource/mcp': self.reply(200,{'resource':base+'/mcp','authorization_servers':[base]})
        elif path.path=='/.well-known/oauth-authorization-server': self.reply(200,{'issuer':base,'authorization_endpoint':base+'/authorize','token_endpoint':base+'/token','registration_endpoint':base+'/register'})
        elif path.path=='/authorize':
            query=urllib.parse.parse_qs(path.query);state=query['state'][0]
            self.server.challenges[state]=query['code_challenge'][0]
            redirect=query['redirect_uri'][0]+'?'+urllib.parse.urlencode({'code':'a+b/=','state':state})
            self.reply(302,headers={'Location':redirect})
        else: self.reply(405)
    def do_POST(self):
        body=self.rfile.read(int(self.headers.get('Content-Length','0')))
        base='http://127.0.0.1:'+str(self.server.server_port)
        if self.path=='/register':
            self.reply(200,{'client_id':'registered-client'});return
        if self.path=='/token':
            query=urllib.parse.parse_qs(body.decode())
            if query.get('resource')!=[base+'/mcp']: self.reply(400);return
            if query.get('grant_type')==['authorization_code']:
                if query.get('code')!=['a+b/=']:self.reply(400);return
                challenge=base64.urlsafe_b64encode(hashlib.sha256(query['code_verifier'][0].encode()).digest()).decode().rstrip('=')
                if challenge not in self.server.challenges.values():self.reply(400);return
            elif query.get('refresh_token')!=['refresh-a']:self.reply(400);return
            self.reply(200,{'access_token':'token-a','refresh_token':'refresh-a','expires_in':3600,'token_type':'Bearer'});return
        if self.path not in ['/mcp','/messages?session=fixture']:self.reply(404);return
        token=self.headers.get('Authorization','')
        if token not in ['Bearer token-a','Bearer token-b']:self.reply(401);return
        account='a' if token=='Bearer token-a' else 'b'
        request=json.loads(body);method=request.get('method')
        if 'id' not in request:self.reply(202);return
        if method=='initialize': result={'protocolVersion':'2025-03-26','capabilities':{'tools':{}},'serverInfo':{'name':'http-fixture','version':'1'}}
        elif method=='tools/list':result={'tools':[{'name':'account_'+account,'description':'Account '+account,'inputSchema':{'type':'object','properties':{},'additionalProperties':False}}]}
        elif method=='tools/call':result={'content':[{'type':'text','text':account}],'structuredContent':{'account':account},'isError':False}
        elif method=='ping':result={}
        else:self.reply(200,{'jsonrpc':'2.0','id':request['id'],'error':{'code':-32601,'message':'unsupported method'}});return
        response={'jsonrpc':'2.0','id':request['id'],'result':result}
        if self.path.startswith('/messages'):self.server.messages.put(response);self.reply(202)
        else:self.reply(200,response)
server=ThreadingHTTPServer(('127.0.0.1',0),Handler);server.challenges={};server.messages=queue.Queue()
print(server.server_port,flush=True);server.serve_forever()
"#).unwrap();
        let mut child = tokio::process::Command::new(which::which("python3").unwrap())
            .args(["-u", script.to_str().unwrap()])
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut port = String::new();
        stdout.read_line(&mut port).await.unwrap();
        Self {
            directory,
            _child: child,
            url: format!("http://127.0.0.1:{}/mcp", port.trim()),
        }
    }
    fn runtime(
        &self,
        owner: &str,
        vault: Arc<crate::common::vault::Vault>,
        auth: Value,
    ) -> Arc<PrincipalMcpRuntime> {
        let workspace = self.directory.path().join(owner);
        let server = workspace.join("mcp/shared");
        std::fs::create_dir_all(&server).unwrap();
        std::fs::write(server.join("server.json"),json!({"schema_version":1,"transport":{"type":"streamable_http","url":self.url},"auth":auth,"health_check_interval_secs":0}).to_string()).unwrap();
        PrincipalMcpRuntime::new(
            PrincipalId(owner.into()),
            workspace,
            self.directory.path().join(format!("cache-{owner}")),
            Some(vault),
            None,
            Arc::new(peko_quota::QuotaMeter::unlimited()),
            None,
        )
    }
}

#[tokio::test]
async fn same_http_endpoint_uses_independent_principal_credentials_and_catalogs() {
    let http = HttpFixture::new().await;
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(crate::common::vault::Vault::for_test(
        vault_dir.path(),
        "http-fixture-passphrase",
    ));
    let tooling = crate::tools::runtime::ToolingRuntime::standalone();
    for owner in ["a", "b"] {
        let runtime = http.runtime(
            owner,
            vault.clone(),
            json!({"type":"bearer","credential":"token"}),
        );
        vault
            .set_credential(&crate::common::vault::Credential::now(
                runtime.namespace("shared"),
                "token",
                crate::common::vault::CredentialKind::BearerToken,
                secrecy::SecretString::new(format!("token-{owner}").into()),
            ))
            .unwrap();
        tooling.bind_mcp(runtime.clone()).await;
        let snapshot = tooling.list_tool_definitions(runtime.owner()).await;
        assert!(
            snapshot
                .definitions
                .iter()
                .any(|tool| tool.name == format!("mcp__shared__account_{owner}")),
            "{}",
            runtime.status().await
        );
        assert!(!snapshot.definitions.iter().any(|tool| tool.name
            == format!(
                "mcp__shared__account_{}",
                if owner == "a" { "b" } else { "a" }
            )));
        let mut call =
            peko_engine::ToolCallSpec::new(format!("mcp__shared__account_{owner}"), json!({}));
        call.principal_id = Some(owner.into());
        call.catalog_binding = snapshot.binding;
        let (text, value, success) = tooling.execute(call).await.unwrap();
        assert!(success, "{text}");
        assert_eq!(value["structuredContent"]["account"], owner);
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn oauth_discovery_registration_pkce_callback_and_refresh_are_owner_scoped() {
    let http = HttpFixture::new().await;
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(crate::common::vault::Vault::for_test(
        vault_dir.path(),
        "oauth-fixture-passphrase",
    ));
    let runtime = http.runtime("oauth-owner", vault.clone(), json!({"type":"oauth"}));
    let context = ToolContext::for_hook_run("run", "call", "MCP").with_principal_id("oauth-owner");
    let result = runtime
        .operate("authenticate", Some("shared"), &json!({}), &context)
        .await
        .unwrap();
    let response = reqwest::Client::new()
        .get(result["authorization_url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    for _ in 0..100 {
        if runtime.authenticating.lock().await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    runtime.prepare_catalog().await.unwrap();
    assert!(
        runtime
            .tools()
            .await
            .iter()
            .any(|(_, tool)| tool.name() == "mcp__shared__account_a"),
        "{}",
        runtime.status().await
    );
    let server = runtime.server("shared").await.unwrap();
    let mut record = vault
        .list_credentials(&crate::common::vault::CredentialFilter {
            namespace: Some(runtime.namespace("shared")),
            ..Default::default()
        })
        .into_iter()
        .find_map(|record| vault.get_credential(&record.id))
        .unwrap();
    assert_eq!(record.metadata["client_id"], "registered-client");
    assert!(!result.to_string().contains("token-a"));
    record.metadata["expires_at"] = json!(0);
    vault.set_credential(&record).unwrap();
    assert_eq!(
        runtime.bearer(&server).await.unwrap().as_deref(),
        Some("token-a")
    );
    assert!(vault
        .list_credentials(&crate::common::vault::CredentialFilter {
            namespace: Some("mcp:principal:another-owner:shared".into()),
            ..Default::default()
        })
        .is_empty());
    runtime.shutdown().await;
}

#[tokio::test]
async fn legacy_sse_uses_the_advertised_message_endpoint() {
    let http = HttpFixture::new().await;
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(crate::common::vault::Vault::for_test(
        vault_dir.path(),
        "sse-fixture-passphrase",
    ));
    let runtime = http.runtime(
        "sse-owner",
        vault.clone(),
        json!({"type":"bearer","credential":"token"}),
    );
    let manifest = runtime.workspace.join("mcp/shared/server.json");
    let mut definition: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    definition["transport"] = json!({"type":"sse","endpoint":http.url.replace("/mcp","/sse")});
    std::fs::write(manifest, definition.to_string()).unwrap();
    vault
        .set_credential(&crate::common::vault::Credential::now(
            runtime.namespace("shared"),
            "token",
            crate::common::vault::CredentialKind::BearerToken,
            secrecy::SecretString::new("token-a".into()),
        ))
        .unwrap();
    runtime.prepare_catalog().await.unwrap();
    let tools = runtime.tools().await;
    assert!(!tools.is_empty(), "{}", runtime.status().await);
    let tool = tools.into_iter().next().unwrap().1;
    let context =
        ToolContext::for_hook_run("run", "call", "account_a").with_principal_id("sse-owner");
    let result = tool
        .execute_with_context(json!({}), &context)
        .await
        .unwrap();
    assert_eq!(result["structuredContent"]["account"], "a");
    runtime.shutdown().await;
}
