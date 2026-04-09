use std::collections::HashMap;
use std::env;
use std::fmt;

#[derive(Clone)]
pub struct ProjectConfig {
    pub repo_url: String,
    pub webhook_secret: String,
    pub bunny_storage_zone: String,
    pub bunny_storage_password: String,
    pub bunny_pull_zone_id: String,
    pub bunny_pull_zone_domain: String,
    pub bunny_api_key: Option<String>,
}

impl fmt::Debug for ProjectConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProjectConfig")
            .field("repo_url", &self.repo_url)
            .field("webhook_secret", &"[REDACTED]")
            .field("bunny_storage_zone", &self.bunny_storage_zone)
            .field("bunny_storage_password", &"[REDACTED]")
            .field("bunny_pull_zone_id", &self.bunny_pull_zone_id)
            .field("bunny_pull_zone_domain", &self.bunny_pull_zone_domain)
            .field(
                "bunny_api_key",
                &self.bunny_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[derive(Clone)]
pub struct Config {
    pub bind_addr: String,
    #[allow(dead_code)]
    pub bunny_api_key: String,
    pub projects: HashMap<String, ProjectConfig>,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("bunny_api_key", &"[REDACTED]")
            .field("projects", &self.projects)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let vars: HashMap<String, String> = env::vars().collect();

        let bind_addr = get_required(&vars, "BIND_ADDR")?;
        let bunny_api_key = get_required(&vars, "BUNNY_API_KEY")?;

        let mut projects = HashMap::new();

        for key in vars.keys() {
            if key.starts_with("PROJECT_") && key.ends_with("_REPO_URL") {
                let project_id = extract_project_id(key)?;
                let project = Self::parse_project(&vars, &project_id)?;
                projects.insert(project_id, project);
            }
        }

        if projects.is_empty() {
            return Err(ConfigError::NoProjects);
        }

        Ok(Config {
            bind_addr,
            bunny_api_key,
            projects,
        })
    }

    fn parse_project(
        vars: &HashMap<String, String>,
        project_id: &str,
    ) -> Result<ProjectConfig, ConfigError> {
        let prefix = format!("PROJECT_{}_", project_id);
        let get = |key: &str| get_project_var(vars, project_id, key);

        let repo_url = get(&format!("{prefix}REPO_URL"))?;
        if !repo_url.starts_with("http://") && !repo_url.starts_with("https://") {
            return Err(ConfigError::InvalidUrl(project_id.to_string(), repo_url));
        }

        let webhook_secret = get(&format!("{prefix}WEBHOOK_SECRET"))?;
        if webhook_secret.len() < 32 {
            return Err(ConfigError::ShortSecret(project_id.to_string()));
        }

        let bunny_storage_zone = get(&format!("{prefix}BUNNY_STORAGE_ZONE"))?;
        let bunny_storage_password = get(&format!("{prefix}BUNNY_STORAGE_PASSWORD"))?;
        let bunny_pull_zone_id = get(&format!("{prefix}BUNNY_PULL_ZONE_ID"))?;

        if bunny_pull_zone_id.parse::<u64>().is_err() {
            return Err(ConfigError::InvalidPullZoneId(project_id.to_string()));
        }

        let bunny_pull_zone_domain = get(&format!("{prefix}BUNNY_PULL_ZONE_DOMAIN"))?;
        let bunny_api_key = vars.get(&format!("{prefix}BUNNY_API_KEY")).cloned();

        Ok(ProjectConfig {
            repo_url,
            webhook_secret,
            bunny_storage_zone,
            bunny_storage_password,
            bunny_pull_zone_id,
            bunny_pull_zone_domain,
            bunny_api_key,
        })
    }

    pub fn validate_and_print(&self) {
        eprintln!("[startup] validating config...");

        let project_ids: Vec<_> = self.projects.keys().cloned().collect();
        eprintln!("[startup] found projects: {}", project_ids.join(", "));

        for id in self.projects.keys() {
            eprintln!("[startup] {}: ok", id);
        }

        eprintln!("[startup] global BUNNY_API_KEY: ok");
        eprintln!(
            "[startup] config valid — starting server on {}",
            self.bind_addr
        );
    }
}

#[derive(Debug)]
pub enum ConfigError {
    MissingVar(String),
    MissingProjectVar(String, String),
    NoProjects,
    InvalidUrl(String, String),
    ShortSecret(String),
    InvalidPullZoneId(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::MissingVar(var) => write!(f, "missing {}", var),
            ConfigError::MissingProjectVar(project, var) => {
                write!(f, "PROJECT_{}_{}", project, var)
            }
            ConfigError::NoProjects => write!(f, "no projects configured"),
            ConfigError::InvalidUrl(project, url) => {
                write!(f, "{}: invalid URL: {}", project, url)
            }
            ConfigError::ShortSecret(project) => {
                write!(
                    f,
                    "{}: WEBHOOK_SECRET must be at least 32 characters",
                    project
                )
            }
            ConfigError::InvalidPullZoneId(project) => {
                write!(f, "{}: BUNNY_PULL_ZONE_ID must be a valid number", project)
            }
        }
    }
}

impl std::error::Error for ConfigError {}

fn extract_project_id(key: &str) -> Result<String, ConfigError> {
    let stripped = key
        .strip_prefix("PROJECT_")
        .and_then(|s| s.strip_suffix("_REPO_URL"))
        .ok_or_else(|| ConfigError::MissingVar(format!("invalid project key: {}", key)))?;
    Ok(stripped.to_string())
}

fn get_required(vars: &HashMap<String, String>, key: &str) -> Result<String, ConfigError> {
    vars.get(key)
        .ok_or_else(|| ConfigError::MissingVar(key.to_string()))
        .cloned()
}

fn get_project_var(
    vars: &HashMap<String, String>,
    project_id: &str,
    key: &str,
) -> Result<String, ConfigError> {
    vars.get(key)
        .ok_or_else(|| ConfigError::MissingProjectVar(project_id.to_string(), key.to_string()))
        .cloned()
}
