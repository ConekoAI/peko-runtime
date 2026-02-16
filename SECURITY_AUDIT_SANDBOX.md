# Security Audit: Sandbox/Allowlist Implementation

**Audit Date:** 2025-02-16  
**Auditor:** Agent Alpha  
**Focus:** ZeroClaw-compatible sandbox and allowlist security
**Status:** 🔴 CRITICAL GAPS IDENTIFIED

---

## Executive Summary

This audit focuses on the sandbox and allowlist mechanisms needed for ZeroClaw compatibility. **Critical security gaps exist** - Pekobot currently lacks proper sandboxing and tool execution controls.

**Risk Level:** 🔴 HIGH  
**Recommendation:** Implement sandbox controls before production use

---

## 1. Tool Execution Sandbox

### 1.1 Current State Analysis

| Feature | ZeroClaw | Pekobot | Gap |
|---------|----------|---------|-----|
| Tool allowlist | ✅ Yes | 🔴 No | Critical |
| Deny list | ✅ Yes | 🔴 No | Critical |
| Execution timeout | ✅ Yes | 🟡 Partial | Medium |
| Resource limits | ✅ Yes | 🔴 No | High |
| Network restrictions | ✅ Yes | 🟡 Partial | Medium |

### 1.2 Required Implementation

```rust
// Required: Tool sandbox configuration
pub struct ToolSandbox {
    /// Allowed tools (empty = all allowed)
    pub allowlist: Vec<String>,
    
    /// Blocked tools (takes precedence)
    pub denylist: Vec<String>,
    
    /// Execution timeout in seconds
    pub timeout_seconds: u64,
    
    /// Maximum memory usage (bytes)
    pub max_memory_bytes: usize,
    
    /// CPU time limit (seconds)
    pub max_cpu_seconds: f64,
    
    /// Network access policy
    pub network_policy: NetworkPolicy,
    
    /// Filesystem access policy
    pub filesystem_policy: FilesystemPolicy,
}

pub enum NetworkPolicy {
    AllowAll,
    BlockAll,
    Allowlist(Vec<String>), // Allowed domains
    Denylist(Vec<String>),  // Blocked domains
}

pub struct FilesystemPolicy {
    pub read_allowlist: Vec<PathBuf>,
    pub write_allowlist: Vec<PathBuf>,
    pub blocklist: Vec<PathBuf>,
}
```

### 1.3 Security Requirements

#### Tool Allowlist Enforcement
```rust
impl ToolSandbox {
    pub fn can_execute(&self, tool_name: &str) -> Result<()> {
        // 1. Check denylist first (takes precedence)
        if self.denylist.contains(&tool_name.to_string()) {
            return Err(anyhow!("Tool '{}' is in denylist", tool_name));
        }
        
        // 2. Check allowlist (if not empty)
        if !self.allowlist.is_empty() && !self.allowlist.contains(&tool_name.to_string()) {
            return Err(anyhow!("Tool '{}' not in allowlist", tool_name));
        }
        
        Ok(())
    }
}
```

**Audit Finding:** Currently no allowlist validation in tool execution.

---

## 2. HTTP Tool Security

### 2.1 Current Implementation Gaps

Location: `src/tools/http.rs`

| Vulnerability | Status | Severity |
|--------------|--------|----------|
| SSRF (Server-Side Request Forgery) | 🔴 Unprotected | CRITICAL |
| DNS rebinding | 🔴 Unprotected | HIGH |
| Open redirect exploitation | 🔴 Unprotected | HIGH |
| No URL validation | 🔴 Missing | CRITICAL |
| No request size limits | 🟡 Partial | MEDIUM |

### 2.2 Required SSRF Protection

```rust
pub struct HttpSandbox {
    /// Blocked IP ranges (private, link-local, etc.)
    pub blocked_ip_ranges: Vec<IpRange>,
    
    /// Allowed URL schemes
    pub allowed_schemes: Vec<String>,
    
    /// Domain allowlist (empty = all allowed)
    pub domain_allowlist: Vec<String>,
    
    /// Domain denylist
    pub domain_denylist: Vec<String>,
    
    /// Maximum response size
    pub max_response_size: usize,
    
    /// Require HTTPS
    pub require_https: bool,
}

impl HttpSandbox {
    /// Validate URL before request
    pub fn validate_url(&self, url: &str) -> Result<Url> {
        let parsed = Url::parse(url)
            .context("Invalid URL")?;
        
        // 1. Validate scheme
        if !self.allowed_schemes.contains(&parsed.scheme().to_string()) {
            return Err(anyhow!("Scheme '{}' not allowed", parsed.scheme()));
        }
        
        // 2. Resolve hostname to IP
        let hostname = parsed.host_str()
            .ok_or_else(|| anyhow!("URL has no hostname"))?;
        
        let ips: Vec<std::net::IpAddr> = dns_lookup::lookup_host(hostname)?
            .collect();
        
        // 3. Check IP against blocked ranges
        for ip in &ips {
            if self.is_ip_blocked(ip) {
                return Err(anyhow!(
                    "IP {} is in blocked range",
                    ip
                ));
            }
        }
        
        // 4. Check domain allowlist/denylist
        if self.is_domain_blocked(hostname) {
            return Err(anyhow!("Domain '{}' is blocked", hostname));
        }
        
        // 5. Validate URL doesn't contain credentials
        if parsed.username() != "" || parsed.password().is_some() {
            return Err(anyhow!("URLs with embedded credentials not allowed"));
        }
        
        Ok(parsed)
    }
    
    fn is_ip_blocked(&self, ip: &std::net::IpAddr) -> bool {
        match ip {
            // IPv4 private ranges
            IpAddr::V4(v4) => {
                v4.is_private() ||      // 10/8, 172.16/12, 192.168/16
                v4.is_loopback() ||     // 127/8
                v4.is_link_local() ||   // 169.254/16
                v4.is_broadcast() ||    // 255.255.255.255
                v4.is_documentation() || // 192.0.2/24, 198.51.100/24, 203.0.113/24
                v4.is_unspecified()     // 0.0.0.0
            }
            // IPv6 private ranges
            IpAddr::V6(v6) => {
                v6.is_loopback() ||
                v6.is_unspecified() ||
                // fc00::/7 (ULA)
                (v6.segments()[0] & 0xfe00) == 0xfc00 ||
                // fe80::/10 (link-local)
                (v6.segments()[0] & 0xffc0) == 0xfe80
            }
        }
    }
}
```

### 2.3 DNS Rebinding Protection

```rust
/// DNS Rebinding attack prevention
pub struct DnsRebindingProtection {
    /// Cache of resolved IPs with TTL
    resolved_cache: HashMap<String, (Vec<IpAddr>, Instant)>,
    /// Minimum TTL for DNS entries
    min_ttl: Duration,
}

impl DnsRebindingProtection {
    /// Resolve URL with rebinding protection
    pub async fn resolve_safe(&mut self,
        url: &str,
    ) -> Result<(Url, Vec<IpAddr>)> {
        let parsed = Url::parse(url)?;
        let hostname = parsed.host_str()
            .ok_or_else(|| anyhow!("No hostname"))?;
        
        // Check cache
        if let Some((ips, resolved_at)) = self.resolved_cache.get(hostname) {
            if resolved_at.elapsed() < self.min_ttl {
                return Ok((parsed, ips.clone()));
            }
        }
        
        // Fresh DNS resolution
        let ips: Vec<IpAddr> = dns_lookup::lookup_host(hostname)?
            .collect();
        
        // Store in cache
        self.resolved_cache.insert(
            hostname.to_string(),
            (ips.clone(), Instant::now()),
        );
        
        Ok((parsed, ips))
    }
}
```

---

## 3. Filesystem Tool Security

### 3.1 Path Traversal Prevention

```rust
pub struct FilesystemSandbox {
    /// Allowed base directories for reads
    pub read_allowlist: Vec<PathBuf>,
    
    /// Allowed base directories for writes
    pub write_allowlist: Vec<PathBuf>,
    
    /// Blocked paths (takes precedence)
    pub blocklist: Vec<PathBuf>,
    
    /// Follow symlinks
    pub follow_symlinks: bool,
    
    /// Maximum file size
    pub max_file_size: usize,
}

impl FilesystemSandbox {
    /// Validate and sanitize path
    pub fn validate_path(
        &self,
        path: &Path,
        access_type: AccessType,
    ) -> Result<PathBuf> {
        // 1. Normalize path (resolve . and ..)
        let normalized = path.canonicalize()
            .or_else(|_| Ok(path.to_path_buf()))?;
        
        // 2. Check blocklist
        for blocked in &self.blocklist {
            if normalized.starts_with(blocked) {
                return Err(anyhow!(
                    "Path '{}' is in blocklist",
                    path.display()
                ));
            }
        }
        
        // 3. Check allowlist
        let allowlist = match access_type {
            AccessType::Read => &self.read_allowlist,
            AccessType::Write => &self.write_allowlist,
        };
        
        if !allowlist.is_empty() {
            let allowed = allowlist.iter()
                .any(|base| normalized.starts_with(base));
            
            if !allowed {
                return Err(anyhow!(
                    "Path '{}' outside allowed directories",
                    path.display()
                ));
            }
        }
        
        // 4. Check for path traversal attempts
        let path_str = path.to_string_lossy();
        if path_str.contains("..") || path_str.contains("~") {
            return Err(anyhow!("Path contains invalid characters"));
        }
        
        Ok(normalized)
    }
}
```

---

## 4. Process Execution Security

### 4.1 Command Injection Prevention

```rust
pub struct ProcessSandbox {
    /// Allowed executables (full paths)
    pub allowed_executables: Vec<PathBuf>,
    
    /// Blocked executables
    pub blocked_executables: Vec<PathBuf>,
    
    /// Environment variable whitelist
    pub env_whitelist: Vec<String>,
    
    /// Working directory restrictions
    pub working_directory_policy: WorkingDirectoryPolicy,
    
    /// Resource limits
    pub resource_limits: ResourceLimits,
}

pub struct ResourceLimits {
    pub max_memory_mb: usize,
    pub max_cpu_seconds: f64,
    pub max_file_descriptors: usize,
    pub max_processes: usize,
}

impl ProcessSandbox {
    /// Execute command with sandboxing
    pub async fn execute(&self,
        program: &str,
        args: &[ String],
        env_vars: HashMap<String, String>,
    ) -> Result<ProcessOutput> {
        // 1. Validate executable
        let program_path = Path::new(program);
        self.validate_executable(program_path)?;
        
        // 2. Sanitize arguments (prevent injection)
        let sanitized_args: Vec<String> = args.iter()
            .map(|arg| self.sanitize_arg(arg))
            .collect();
        
        // 3. Filter environment variables
        let filtered_env: HashMap<String, String> = env_vars.into_iter()
            .filter(|(k, _)| self.env_whitelist.contains(k))
            .collect();
        
        // 4. Set up resource limits using cgroups/ulimit
        let mut cmd = TokioCommand::new(program);
        cmd.args(&sanitized_args)
            .env_clear()
            .envs(&filtered_env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        
        // Apply timeout
        let output = timeout(
            Duration::from_secs(self.resource_limits.max_cpu_seconds as u64),
            cmd.output(),
        ).await??;
        
        Ok(output)
    }
    
    fn sanitize_arg(&self, arg: &str) -> String {
        // Remove shell metacharacters
        arg.chars()
            .filter(|c| !matches!(c, ';' | '|' | '&' | '$' | '`' | '(' | ')' | '<' | '>'))
            .collect()
    }
}
```

---

## 5. Configuration Security

### 5.1 Secure Defaults

```toml
# pekobot.toml - Secure defaults

[sandbox]
# Default: block everything, allow explicitly
default_policy = "deny"

[sandbox.tools]
# Empty allowlist = no tools allowed by default
allowlist = []

# Common dangerous tools to block
denylist = [
    "exec",
    "eval",
    "shell",
    "system",
]

[sandbox.http]
require_https = true
max_response_size = "10MB"
timeout_seconds = 30

[sandbox.http.blocked_ranges]
# Private IP ranges
ipv4 = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "127.0.0.0/8"]
ipv6 = ["fc00::/7", "fe80::/10", "::1/128"]

[sandbox.filesystem]
read_allowlist = ["/tmp/pekobot"]
write_allowlist = ["/tmp/pekobot"]
blocklist = [
    "/etc/passwd",
    "/etc/shadow",
    "/.env",
    "~/.ssh",
]
follow_symlinks = false
max_file_size = "100MB"

[sandbox.process]
allowed_executables = []
max_memory_mb = 512
max_cpu_seconds = 60.0
```

---

## 6. Audit Findings Summary

### Critical (Fix Before Production)

| ID | Finding | Impact | Mitigation |
|----|---------|--------|------------|
| SANDBOX-001 | No tool allowlist enforcement | Arbitrary tool execution | Implement `ToolSandbox::can_execute()` |
| SSRF-001 | HTTP tool vulnerable to SSRF | Internal network access | Implement IP validation, DNS rebinding protection |
| PATH-001 | No path traversal protection | Filesystem escape | Implement path canonicalization, allowlist validation |

### High Priority

| ID | Finding | Impact | Mitigation |
|----|---------|--------|------------|
| CMD-001 | No command injection prevention | Code execution | Implement argument sanitization |
| ENV-001 | No environment variable filtering | Credential exposure | Implement env whitelist |
| NET-001 | No request size limits | DoS via large responses | Add size limits |

### Medium Priority

| ID | Finding | Impact | Mitigation |
|----|---------|--------|------------|
| RATE-001 | No rate limiting on tools | Resource exhaustion | Implement per-tool rate limits |
| LOG-001 | Tool arguments logged in full | Credential exposure | Sanitize logs |

---

## 7. Implementation Checklist

### Phase 1: Critical Security (Week 1)
- [ ] Implement `ToolSandbox` with allowlist/denylist
- [ ] Add SSRF protection to HTTP tool
- [ ] Add path traversal protection to filesystem tool
- [ ] Add secure defaults to configuration

### Phase 2: High Priority (Week 2)
- [ ] Implement process sandboxing
- [ ] Add command injection prevention
- [ ] Implement environment variable filtering
- [ ] Add request/response size limits

### Phase 3: Hardening (Week 3)
- [ ] Add rate limiting
- [ ] Implement audit logging
- [ ] Add resource limits (cgroups)
- [ ] Create security documentation

### Phase 4: Testing (Week 4)
- [ ] Write security-focused tests
- [ ] Perform penetration testing
- [ ] Audit with cargo-audit
- [ ] Document security configuration

---

## 8. Testing Security Controls

```rust
#[cfg(test)]
mod sandbox_security_tests {
    use super::*;
    
    #[test]
    fn test_tool_denylist_blocks() {
        let sandbox = ToolSandbox {
            denylist: vec!["dangerous_tool".to_string()],
            ..Default::default()
        };
        
        assert!(sandbox.can_execute("dangerous_tool").is_err());
        assert!(sandbox.can_execute("safe_tool").is_ok());
    }
    
    #[test]
    fn test_ssrf_protection_blocks_private_ips() {
        let http_sandbox = HttpSandbox::default();
        
        // Should block private IPs
        assert!(http_sandbox.validate_url("http://192.168.1.1/").is_err());
        assert!(http_sandbox.validate_url("http://10.0.0.1/").is_err());
        assert!(http_sandbox.validate_url("http://127.0.0.1/").is_err());
        
        // Should allow public IPs
        assert!(http_sandbox.validate_url("https://api.openai.com/").is_ok());
    }
    
    #[test]
    fn test_path_traversal_protection() {
        let fs_sandbox = FilesystemSandbox {
            read_allowlist: vec![PathBuf::from("/allowed")],
            ..Default::default()
        };
        
        // Should block traversal attempts
        assert!(fs_sandbox.validate_path(
            Path::new("/allowed/../../../etc/passwd"),
            AccessType::Read
        ).is_err());
        
        // Should allow valid paths
        assert!(fs_sandbox.validate_path(
            Path::new("/allowed/file.txt"),
            AccessType::Read
        ).is_ok());
    }
    
    #[test]
    fn test_command_injection_prevention() {
        let sandbox = ProcessSandbox::default();
        
        let malicious = "file.txt; rm -rf /";
        let sanitized = sandbox.sanitize_arg(malicious);
        
        assert!(!sanitized.contains(';'));
        assert!(!sanitized.contains('|'));
        assert!(!sanitized.contains('&'));
    }
}
```

---

## 9. ZeroClaw Compatibility

### Feature Mapping

| ZeroClaw Feature | Pekobot Status | Notes |
|-----------------|---------------|-------|
| `tool_allowlist` | 🔴 Missing | Must implement |
| `tool_denylist` | 🔴 Missing | Must implement |
| `sandbox_mode` | 🔴 Missing | Must implement |
| `max_tool_calls` | 🔴 Missing | Add to config |
| `timeout_seconds` | 🟡 Partial | Exists but not enforced |

### Migration Guide Section

```markdown
## Migrating Tool Security from ZeroClaw

### Configuration Changes

ZeroClaw:
```yaml
tool_allowlist:
  - http_request
  - file_read
  
tool_denylist:
  - exec
  - eval

sandbox_mode: strict
```

Pekobot:
```toml
[sandbox.tools]
allowlist = ["http", "filesystem"]
denylist = ["exec", "eval", "shell"]

[sandbox.http]
require_https = true
timeout_seconds = 30

[sandbox.filesystem]
read_allowlist = ["/data"]
write_allowlist = ["/tmp"]
```
```

---

**Audit Status:** 🔴 CRITICAL SECURITY GAPS  
**Next Review:** After Phase 1 implementation  
**Distribution:** Agent Gamma (Implementation), Agent Beta (Documentation)
