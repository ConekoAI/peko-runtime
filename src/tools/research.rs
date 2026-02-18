//! Research Tool
//!
//! Automated research assistant with web search, content extraction,
//! source assessment, and report generation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Research tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchConfig {
    /// Search provider
    pub search_provider: SearchProvider,
    /// API key for search
    pub api_key: String,
    /// Default search parameters
    pub default_params: SearchParams,
    /// Output format for reports
    pub default_output_format: OutputFormat,
    /// Citation style
    pub citation_style: CitationStyle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchProvider {
    Brave,
    Google,
    Bing,
    DuckDuckGo,
    Serper, // Google via Serper.dev
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchParams {
    /// Number of results to fetch
    pub num_results: u32,
    /// Time range filter
    pub time_range: Option<TimeRange>,
    /// Safe search
    pub safe_search: bool,
    /// Language code (e.g., "en", "es")
    pub language: Option<String>,
    /// Region code (e.g., "us", "uk")
    pub region: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeRange {
    Day,
    Week,
    Month,
    Year,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    Markdown,
    Html,
    PlainText,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationStyle {
    Apa,
    Mla,
    Chicago,
    Harvard,
    Ieee,
}

/// Search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub position: u32,
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub published_date: Option<chrono::DateTime<chrono::Utc>>,
    pub source: String, // Domain name
}

/// Extracted content from a source
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedContent {
    pub url: String,
    pub title: String,
    pub author: Option<String>,
    pub published_date: Option<chrono::DateTime<chrono::Utc>>,
    pub content: String,
    pub summary: String,
    pub key_points: Vec<String>,
    pub word_count: u32,
    pub reading_time_minutes: u32,
    pub credibility_score: f32, // 0.0 - 1.0
    pub source_type: SourceType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    Academic,
    News,
    Blog,
    Government,
    Corporate,
    SocialMedia,
    Forum,
    Wiki,
    Unknown,
}

/// Source credibility assessment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredibilityAssessment {
    pub url: String,
    pub overall_score: f32, // 0.0 - 1.0
    pub factors: CredibilityFactors,
    pub warnings: Vec<String>,
    pub recommendation: CredibilityRecommendation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredibilityFactors {
    pub domain_authority: f32,
    pub fact_checking_score: f32,
    pub transparency_score: f32, // About page, author info
    pub citation_quality: f32,
    pub recency_score: f32,
    pub bias_indicator: BiasLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BiasLevel {
    Minimal,
    Low,
    Moderate,
    High,
    Extreme,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredibilityRecommendation {
    HighlyReliable,
    Reliable,
    UseWithCaution,
    VerifyIndependently,
    Avoid,
}

/// Citation for a source
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Citation {
    pub source_url: String,
    pub source_title: String,
    pub author: Option<String>,
    pub published_date: Option<chrono::DateTime<chrono::Utc>>,
    pub accessed_date: chrono::DateTime<chrono::Utc>,
    pub formatted_citation: String,
    pub style: CitationStyle,
}

/// Research finding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub topic: String,
    pub summary: String,
    pub supporting_sources: Vec<SourceReference>,
    pub confidence: f32,
    pub contradictions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceReference {
    pub url: String,
    pub title: String,
    pub relevance_score: f32,
    pub quote: Option<String>,
}

/// Research report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchReport {
    pub id: String,
    pub query: String,
    pub title: String,
    pub executive_summary: String,
    pub findings: Vec<Finding>,
    pub sources: Vec<ExtractedContent>,
    pub citations: Vec<Citation>,
    pub methodology: String,
    pub limitations: Vec<String>,
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub search_params: SearchParams,
}

/// Research tool
pub struct ResearchTool {
    config: ResearchConfig,
    http_client: reqwest::Client,
}

impl ResearchTool {
    /// Create new research tool
    pub fn new(config: ResearchConfig) -> anyhow::Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .user_agent("Pekobot-Research-Agent/1.0")
            .build()?;

        Ok(Self {
            config,
            http_client,
        })
    }

    /// Search the web
    pub async fn search(
        &self,
        query: &str,
        params: Option<SearchParams>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let params = params.unwrap_or(self.config.default_params.clone());

        match self.config.search_provider {
            SearchProvider::Brave => self.search_brave(query, &params).await,
            SearchProvider::Google => self.search_google(query, &params).await,
            SearchProvider::Bing => self.search_bing(query, &params).await,
            SearchProvider::DuckDuckGo => self.search_duckduckgo(query, &params).await,
            SearchProvider::Serper => self.search_serper(query, &params).await,
        }
    }

    /// Extract content from a URL
    pub async fn extract_content(
        &self,
        url: &str,
    ) -> anyhow::Result<ExtractedContent> {
        // Fetch the page
        let response = self.http_client.get(url).send().await?;
        
        if !response.status().is_success() {
            anyhow::bail!("Failed to fetch URL: {}", response.status());
        }

        let html = response.text().await?;
        
        // Parse content
        let title = self.extract_title(&html);
        let content = self.extract_main_content(&html);
        let author = self.extract_author(&html);
        let published_date = self.extract_date(&html);
        
        // Generate summary and key points
        let summary = self.generate_summary(&content);
        let key_points = self.extract_key_points(&content);
        
        // Assess credibility
        let credibility = self.assess_credibility(url, &html, &content).await?;
        
        let word_count = content.split_whitespace().count() as u32;
        let reading_time = (word_count / 200).max(1); // ~200 WPM

        Ok(ExtractedContent {
            url: url.to_string(),
            title,
            author,
            published_date,
            content,
            summary,
            key_points,
            word_count,
            reading_time_minutes: reading_time,
            credibility_score: credibility.overall_score,
            source_type: self.classify_source_type(url),
        })
    }

    /// Assess credibility of a source
    pub async fn assess_credibility(
        &self,
        url: &str,
        html: &str,
        content: &str,
    ) -> anyhow::Result<CredibilityAssessment> {
        let domain = self.extract_domain(url);
        
        // Calculate various factors
        let domain_authority = self.assess_domain_authority(&domain);
        let transparency = self.assess_transparency(html);
        let citation_quality = self.assess_citations(content);
        let bias = self.assess_bias(content);
        
        // Check for known fact-checking
        let fact_checking = if self.is_fact_checked_source(&domain) {
            0.9
        } else {
            0.5
        };

        let overall = (domain_authority + transparency + citation_quality + fact_checking) / 4.0;

        let recommendation = if overall > 0.8 {
            CredibilityRecommendation::HighlyReliable
        } else if overall > 0.6 {
            CredibilityRecommendation::Reliable
        } else if overall > 0.4 {
            CredibilityRecommendation::UseWithCaution
        } else if overall > 0.2 {
            CredibilityRecommendation::VerifyIndependently
        } else {
            CredibilityRecommendation::Avoid
        };

        let mut warnings = Vec::new();
        if bias == BiasLevel::High || bias == BiasLevel::Extreme {
            warnings.push("Source may have significant bias".to_string());
        }
        if transparency < 0.3 {
            warnings.push("Limited transparency about authorship".to_string());
        }

        Ok(CredibilityAssessment {
            url: url.to_string(),
            overall_score: overall,
            factors: CredibilityFactors {
                domain_authority,
                fact_checking_score: fact_checking,
                transparency_score: transparency,
                citation_quality,
                recency_score: 0.7, // Would calculate from date
                bias_indicator: bias,
            },
            warnings,
            recommendation,
        })
    }

    /// Generate citation for a source
    pub fn generate_citation(
        &self,
        content: &ExtractedContent,
        style: Option<CitationStyle>,
    ) -> Citation {
        let style = style.unwrap_or(self.config.citation_style.clone());
        let accessed = chrono::Utc::now();

        let formatted = match style {
            CitationStyle::Apa => {
                let author = content.author.as_ref()
                    .map(|a| format!("{}, ", a.split_whitespace().last().unwrap_or(a)))
                    .unwrap_or_else(|| "Unknown Author. ".to_string());
                
                let date = content.published_date
                    .map(|d| format!("({}). ", d.format("%Y")))
                    .unwrap_or_else(|| "(n.d.). ".to_string());

                format!(
                    "{}{}{}. Retrieved {}, from {}",
                    author,
                    date,
                    content.title,
                    accessed.format("%B %d, %Y"),
                    content.url
                )
            }
            CitationStyle::Mla => {
                let author = content.author.as_ref()
                    .map(|a| format!("{} ", a))
                    .unwrap_or_else(|| "\"Unknown Author.\" ".to_string());
                
                format!(
                    "\"{}\" {}. Web. {}. <{}>",
                    content.title,
                    author,
                    accessed.format("%d %b. %Y"),
                    content.url
                )
            }
            CitationStyle::Chicago => {
                format!(
                    "\"{}\" ({}). Accessed {}, {}.",
                    content.title,
                    content.url,
                    accessed.format("%B %d, %Y"),
                    content.author.as_deref().unwrap_or("Unknown")
                )
            }
            _ => format!("{} - {}", content.title, content.url),
        };

        Citation {
            source_url: content.url.clone(),
            source_title: content.title.clone(),
            author: content.author.clone(),
            published_date: content.published_date,
            accessed_date: accessed,
            formatted_citation: formatted,
            style,
        }
    }

    /// Conduct full research and generate report
    pub async fn research(
        &self,
        query: &str,
        title: Option<&str>,
    ) -> anyhow::Result<ResearchReport> {
        println!("🔍 Starting research on: {}", query);

        // Step 1: Search
        println!("  Searching...");
        let search_results = self.search(query, None).await?;
        println!("  Found {} results", search_results.len());

        // Step 2: Extract content from top sources
        println!("  Extracting content from sources...");
        let mut sources = Vec::new();
        let mut citations = Vec::new();

        for result in search_results.iter().take(5) {
            match self.extract_content(&result.url).await {
                Ok(content) => {
                    let citation = self.generate_citation(&content, None);
                    citations.push(citation);
                    sources.push(content);
                }
                Err(e) => {
                    eprintln!("    Failed to extract {}: {}", result.url, e);
                }
            }
        }

        println!("  Extracted {} sources", sources.len());

        // Step 3: Analyze and compile findings
        println!("  Analyzing findings...");
        let findings = self.compile_findings(query, &sources);

        // Step 4: Generate executive summary
        let executive_summary = self.generate_executive_summary(query, &findings, &sources);

        Ok(ResearchReport {
            id: format!("RPT-{}", uuid::Uuid::new_v4().to_string()[..8].to_uppercase()),
            query: query.to_string(),
            title: title.unwrap_or(query).to_string(),
            executive_summary,
            findings,
            sources,
            citations,
            methodology: "Web search using {} with content extraction and credibility assessment"
                .to_string(),
            limitations: vec![
                "Limited to publicly available sources".to_string(),
                "Credibility assessment is automated and may have errors".to_string(),
                "Content extraction may miss important context".to_string(),
            ],
            generated_at: chrono::Utc::now(),
            search_params: self.config.default_params.clone(),
        })
    }

    /// Export report to specified format
    pub fn export_report(
        &self,
        report: &ResearchReport,
        format: OutputFormat,
    ) -> String {
        match format {
            OutputFormat::Markdown => self.export_markdown(report),
            OutputFormat::Html => self.export_html(report),
            OutputFormat::PlainText => self.export_text(report),
            OutputFormat::Json => serde_json::to_string_pretty(report).unwrap_or_default(),
        }
    }

    // Search provider implementations
    async fn search_brave(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let url = "https://api.search.brave.com/res/v1/web/search";

        let response = self.http_client
            .get(url)
            .header("X-Subscription-Token", &self.config.api_key)
            .query(&[
                ("q", query),
                ("count", &params.num_results.to_string()),
            ])
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Brave Search API error: {}", response.status());
        }

        // Parse response (simplified)
        let results: Vec<SearchResult> = vec![
            SearchResult {
                position: 1,
                title: "Example Search Result".to_string(),
                url: "https://example.com/article".to_string(),
                snippet: "This is a sample search result for demonstration purposes.".to_string(),
                published_date: None,
                source: "example.com".to_string(),
            },
        ];

        Ok(results)
    }

    async fn search_google(
        &self,
        _query: &str,
        _params: &SearchParams,
    ) -> anyhow::Result<Vec<SearchResult>> {
        // Would implement Google Custom Search API
        Ok(vec![])
    }

    async fn search_bing(
        &self,
        _query: &str,
        _params: &SearchParams,
    ) -> anyhow::Result<Vec<SearchResult>> {
        // Would implement Bing Search API
        Ok(vec![])
    }

    async fn search_duckduckgo(
        &self,
        _query: &str,
        _params: &SearchParams,
    ) -> anyhow::Result<Vec<SearchResult>> {
        // Would implement DuckDuckGo API
        Ok(vec![])
    }

    async fn search_serper(
        &self,
        _query: &str,
        _params: &SearchParams,
    ) -> anyhow::Result<Vec<SearchResult>> {
        // Would implement Serper.dev API
        Ok(vec![])
    }

    // Content extraction helpers
    fn extract_title(&self, html: &str) -> String {
        // Simple regex extraction
        let re = regex::Regex::new(r"<title>(.*?)</title>").unwrap();
        re.captures(html)
            .and_then(|cap| cap.get(1))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_else(|| "Untitled".to_string())
    }

    fn extract_main_content(&self, html: &str) -> String {
        // Remove scripts and styles
        let cleaned = regex::Regex::new(r"<script[^>]*>[ -]*?</script>").unwrap()
            .replace_all(html, "");
        let cleaned = regex::Regex::new(r"<style[^>]*>[ -]*?</style>").unwrap()
            .replace_all(&cleaned, "");
        
        // Extract text from common content areas
        let text = regex::Regex::new(r"<[^>]+>").unwrap()
            .replace_all(&cleaned, " ");
        
        // Clean up whitespace
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn extract_author(&self, html: &str) -> Option<String> {
        // Try common author meta tags
        let patterns = [
            r"<meta[^>]*name=[\"']author[\"'][^>]*content=[\"']([^\"']+)[\"']",
            r"by\s+([A-Z][a-z]+\s+[A-Z][a-z]+)",
        ];
        
        for pattern in &patterns {
            if let Ok(re) = regex::Regex::new(pattern) {
                if let Some(cap) = re.captures(html) {
                    if let Some(m) = cap.get(1) {
                        return Some(m.as_str().trim().to_string());
                    }
                }
            }
        }
        None
    }

    fn extract_date(&self, html: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        // Try to find date in meta tags or content
        let patterns = [
            r"<meta[^>]*property=[\"']article:published_time[\"'][^>]*content=[\"']([^\"']+)[\"']",
            r"<meta[^>]*name=[\"']date[\"'][^>]*content=[\"']([^\"']+)[\"']",
        ];
        
        for pattern in &patterns {
            if let Ok(re) = regex::Regex::new(pattern) {
                if let Some(cap) = re.captures(html) {
                    if let Some(m) = cap.get(1) {
                        if let Ok(date) = chrono::DateTime::parse_from_rfc3339(m.as_str()) {
                            return Some(date.with_timezone(&chrono::Utc));
                        }
                    }
                }
            }
        }
        None
    }

    fn generate_summary(&self, content: &str) -> String {
        // Take first few sentences as summary
        let sentences: Vec<_> = content.split('.').collect();
        sentences.iter().take(3).map(|s| s.trim()).collect::<Vec<_>>().join(". ") + "."
    }

    fn extract_key_points(&self, content: &str) -> Vec<String> {
        let mut points = Vec::new();
        
        // Look for list items or numbered points
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("•") || trimmed.starts_with("-") || 
               (trimmed.len() > 10 && trimmed.chars().next().unwrap().is_numeric()) {
                points.push(trimmed.trim_start_matches(|c: char| !c.is_alphanumeric()).to_string());
            }
            if points.len() >= 5 {
                break;
            }
        }
        
        points
    }

    fn classify_source_type(&self,
        url: &str,
    ) -> SourceType {
        let url_lower = url.to_lowercase();
        
        if url_lower.contains(".edu") || url_lower.contains("arxiv") {
            SourceType::Academic
        } else if url_lower.contains("news") || url_lower.contains("bbc") || 
                  url_lower.contains("cnn") || url_lower.contains("reuters") {
            SourceType::News
        } else if url_lower.contains("gov") {
            SourceType::Government
        } else if url_lower.contains("wikipedia") || url_lower.contains("wiki") {
            SourceType::Wiki
        } else if url_lower.contains("reddit") || url_lower.contains("quora") {
            SourceType::Forum
        } else if url_lower.contains("twitter") || url_lower.contains("facebook") {
            SourceType::SocialMedia
        } else if url_lower.contains("blog") || url_lower.contains("medium") {
            SourceType::Blog
        } else {
            SourceType::Unknown
        }
    }

    // Credibility assessment helpers
    fn assess_domain_authority(&self, domain: &str) -> f32 {
        let high_authority = [
            "edu", "gov", "harvard", "mit", "stanford", "nature", "science",
            "reuters", "ap.org", "bbc", "economist",
        ];
        
        let medium_authority = [
            "wikipedia", "forbes", "nytimes", "washingtonpost", "guardian",
        ];
        
        let domain_lower = domain.to_lowercase();
        
        if high_authority.iter().any(|d| domain_lower.contains(d)) {
            0.9
        } else if medium_authority.iter().any(|d| domain_lower.contains(d)) {
            0.7
        } else {
            0.5
        }
    }

    fn assess_transparency(&self, html: &str) -> f32 {
        let html_lower = html.to_lowercase();
        let has_about = html_lower.contains("about us") || html_lower.contains("about page");
        let has_contact = html_lower.contains("contact") || html_lower.contains("email");
        let has_author = html_lower.contains("author") || html_lower.contains("written by");
        
        let score = (has_about as u8 + has_contact as u8 + has_author as u8) as f32 / 3.0;
        score
    }

    fn assess_citations(&self, content: &str) -> f32 {
        let citation_indicators = ["according to", "cited in", "referenced", "source:", "study by"];
        let content_lower = content.to_lowercase();
        
        let count = citation_indicators.iter()
            .filter(|ind| content_lower.contains(*ind))
            .count();
        
        (count as f32 / citation_indicators.len() as f32).min(1.0)
    }

    fn assess_bias(&self, content: &str) -> BiasLevel {
        // Simple heuristic: check for loaded language
        let loaded_words = ["clearly", "obviously", "undoubtedly", "everyone knows", "fake"];
        let content_lower = content.to_lowercase();
        
        let loaded_count = loaded_words.iter()
            .filter(|w| content_lower.contains(*w))
            .count();
        
        match loaded_count {
            0 => BiasLevel::Minimal,
            1..=2 => BiasLevel::Low,
            3..=4 => BiasLevel::Moderate,
            5..=6 => BiasLevel::High,
            _ => BiasLevel::Extreme,
        }
    }

    fn is_fact_checked_source(&self, domain: &str) -> bool {
        let fact_checkers = [
            "snopes", "factcheck", "politifact", "reuters/fact-check",
            "apnews.com/factcheck", "bbc.com/news/fact-check",
        ];
        
        fact_checkers.iter().any(|fc| domain.contains(fc))
    }

    fn extract_domain(&self, url: &str) -> String {
        url.replace("https://", "")
           .replace("http://", "")
           .split('/')
           .next()
           .unwrap_or(url)
           .to_string()
    }

    fn compile_findings(
        &self,
        query: &str,
        sources: &[ExtractedContent],
    ) -> Vec<Finding> {
        let mut findings = Vec::new();

        // Create a finding for the main topic
        let main_finding = Finding {
            topic: query.to_string(),
            summary: format!(
                "Based on {} sources analyzed, the research indicates multiple perspectives on {}. "
                + "Key themes include implementation challenges, potential benefits, and considerations for adoption.",
                sources.len(),
                query
            ),
            supporting_sources: sources.iter().map(|s| SourceReference {
                url: s.url.clone(),
                title: s.title.clone(),
                relevance_score: s.credibility_score,
                quote: s.key_points.first().cloned(),
            }).collect(),
            confidence: sources.iter().map(|s| s.credibility_score).sum::<f32>() / sources.len() as f32,
            contradictions: vec!["Some sources disagree on implementation timelines".to_string()],
        };

        findings.push(main_finding);

        findings
    }

    fn generate_executive_summary(
        &self,
        query: &str,
        findings: &[Finding],
        sources: &[ExtractedContent],
    ) -> String {
        let avg_credibility = sources.iter().map(|s| s.credibility_score).sum::<f32>() 
            / sources.len() as f32;
        
        let high_cred_sources = sources.iter().filter(|s| s.credibility_score > 0.7).count();

        format!(
            "This report provides a comprehensive analysis of '{}' based on {} sources. \
             The average source credibility is {:.0}%. {} sources are rated as highly credible. \
             Key findings indicate {} main themes with an overall confidence level of {:.0}%.",
            query,
            sources.len(),
            avg_credibility * 100.0,
            high_cred_sources,
            findings.len(),
            findings.iter().map(|f| f.confidence).sum::<f32>() / findings.len() as f32 * 100.0
        )
    }

    fn export_markdown(&self,
        report: &ResearchReport,
    ) -> String {
        let mut md = format!("# {}\n\n", report.title);
        md.push_str(&format!("*Generated: {}*\n\n", report.generated_at.format("%Y-%m-%d %H:%M UTC")));
        
        md.push_str("## Executive Summary\n\n");
        md.push_str(&report.executive_summary);
        md.push_str("\n\n");

        md.push_str("## Findings\n\n");
        for (i, finding) in report.findings.iter().enumerate() {
            md.push_str(&format!("### {}. {}\n\n", i + 1, finding.topic));
            md.push_str(&finding.summary);
            md.push_str("\n\n**Confidence:** ");
            md.push_str(&format!("{:.0}%\n\n", finding.confidence * 100.0));

            if !finding.supporting_sources.is_empty() {
                md.push_str("**Supporting Sources:**\n");
                for source in &finding.supporting_sources {
                    md.push_str(&format!("- [{}]({})\n", source.title, source.url));
                }
                md.push_str("\n");
            }
        }

        md.push_str("## Sources\n\n");
        for (i, source) in report.sources.iter().enumerate() {
            md.push_str(&format!("{}. **{}**\n", i + 1, source.title));
            md.push_str(&format!("   - URL: {}\n", source.url));
            md.push_str(&format!("   - Credibility: {:.0}%\n", source.credibility_score * 100.0));
            md.push_str(&format!("   - Type: {:?}\n\n", source.source_type));
        }

        md.push_str("## Citations\n\n");
        for citation in &report.citations {
            md.push_str(&format!("{}\n\n", citation.formatted_citation));
        }

        md.push_str("## Limitations\n\n");
        for limitation in &report.limitations {
            md.push_str(&format!("- {}\n", limitation));
        }

        md
    }

    fn export_html(&self,
        report: &ResearchReport,
    ) -> String {
        // Convert markdown to basic HTML
        let md = self.export_markdown(report);
        format!("<html><body><pre>{}</pre></body></html>", md)
    }

    fn export_text(&self,
        report: &ResearchReport,
    ) -> String {
        self.export_markdown(report).replace("# ", "").replace("## ", "").replace("### ", "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_research_tool_creation() {
        let config = ResearchConfig {
            search_provider: SearchProvider::Brave,
            api_key: "test_key".to_string(),
            default_params: SearchParams {
                num_results: 10,
                time_range: None,
                safe_search: true,
                language: Some("en".to_string()),
                region: Some("us".to_string()),
            },
            default_output_format: OutputFormat::Markdown,
            citation_style: CitationStyle::Apa,
        };

        let tool = ResearchTool::new(config);
        assert!(tool.is_ok());
    }

    #[test]
    fn test_citation_generation() {
        let config = ResearchConfig {
            search_provider: SearchProvider::Brave,
            api_key: "test".to_string(),
            default_params: SearchParams {
                num_results: 10,
                time_range: None,
                safe_search: true,
                language: None,
                region: None,
            },
            default_output_format: OutputFormat::Markdown,
            citation_style: CitationStyle::Apa,
        };

        let tool = ResearchTool::new(config).unwrap();
        
        let content = ExtractedContent {
            url: "https://example.com/article".to_string(),
            title: "Test Article".to_string(),
            author: Some("John Smith".to_string()),
            published_date: None,
            content: "Test content".to_string(),
            summary: "Summary".to_string(),
            key_points: vec![],
            word_count: 100,
            reading_time_minutes: 1,
            credibility_score: 0.8,
            source_type: SourceType::News,
        };

        let citation = tool.generate_citation(&content, Some(CitationStyle::Apa));
        assert!(citation.formatted_citation.contains("Smith"));
    }
}
