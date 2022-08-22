use std::collections::HashSet;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub processes: Vec<ProcessConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ProcessConfig {
    pub name: String,

    #[serde(default)]
    pub pre: Option<CommandConfig>,

    #[serde(default)]
    pub run: Option<CommandConfig>,

    #[serde(default)]
    pub stop: StopMechanism,

    #[serde(default)]
    pub post: Option<CommandConfig>,
}

#[derive(Clone, Eq, PartialEq, Debug, Deserialize)]
#[serde(untagged)]
pub enum StopMechanism {
    Signal(SignalConfig),

    Command(CommandConfig),
}

impl Default for StopMechanism {
    fn default() -> Self {
        StopMechanism::Signal(SignalConfig::SIGTERM)
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Deserialize)]
pub enum SignalConfig {
    SIGINT,
    SIGQUIT,
    SIGTERM,
}

impl From<SignalConfig> for nix::sys::signal::Signal {
    fn from(signal: SignalConfig) -> Self {
        match signal {
            SignalConfig::SIGINT => Self::SIGINT,
            SignalConfig::SIGQUIT => Self::SIGQUIT,
            SignalConfig::SIGTERM => Self::SIGTERM,
        }
    }
}

impl From<&SignalConfig> for nix::sys::signal::Signal {
    fn from(signal: &SignalConfig) -> Self {
        match signal {
            SignalConfig::SIGINT => Self::SIGINT,
            SignalConfig::SIGQUIT => Self::SIGQUIT,
            SignalConfig::SIGTERM => Self::SIGTERM,
        }
    }
}

/// Configuration for a command, its arguments, and any execution
/// properties (such as the user under which to run the command, or the
/// environment variables to pass through to the command).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(from = "CommandLineConfig")]
pub struct CommandConfig {
    /// User to run this command as, otherwise run the command as the
    /// user that executed Ground Control (most likely `root`).
    pub user: Option<String>,

    /// Environment variables to pass through to the command.
    pub env_vars: HashSet<String>,

    /// Program to execute.
    pub program: String,

    /// Arguments to pass to the program.
    pub args: Vec<String>,
}

#[derive(Clone, Eq, PartialEq, Debug, Deserialize)]
#[serde(untagged)]
enum CommandLineConfig {
    Simple(CommandLine),

    Detailed(DetailedCommandLine),
}

impl From<CommandLineConfig> for CommandConfig {
    fn from(config: CommandLineConfig) -> Self {
        match config {
            CommandLineConfig::Simple(config) => {
                let (program, args) = config.program_and_args();
                Self {
                    user: None,
                    env_vars: Default::default(),
                    program,
                    args,
                }
            }
            CommandLineConfig::Detailed(config) => {
                let (program, args) = config.command.program_and_args();
                Self {
                    user: config.user,
                    env_vars: config.env_vars,
                    program,
                    args,
                }
            }
        }
    }
}

#[derive(Clone, Eq, PartialEq, Debug, Deserialize)]
#[serde(untagged)]
enum CommandLine {
    CommandString(String),

    CommandVector(Vec<String>),
}

impl CommandLine {
    /// Parse the Command Line into the program to execute, and the
    /// arguments to that program.
    fn program_and_args(&self) -> (String, Vec<String>) {
        match self {
            CommandLine::CommandString(line) => {
                // TODO: This won't handle quoted arguments with spaces
                // (for example), so really we should parse this using a
                // more correct, shell-like parser. OTOH, we could just
                // say that anything complicated needs to use the vector
                // format...
                let mut elems = line.split(' ');

                let program = elems
                    .next()
                    .expect("Command line must not be empty")
                    .to_string();
                let args = elems.map(|s| s.to_string()).collect();

                (program, args)
            }

            CommandLine::CommandVector(v) => {
                let program = v[0].to_string();
                let args = v[1..].to_vec();

                (program, args)
            }
        }
    }
}

#[derive(Clone, Eq, PartialEq, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct DetailedCommandLine {
    #[serde(default)]
    user: Option<String>,

    #[serde(default)]
    env_vars: HashSet<String>,

    command: CommandLine,
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize, PartialEq)]
    struct StopMechanismTest {
        stop: StopMechanism,
    }

    #[test]
    fn supports_signal_names_in_stop() {
        let toml = r#"stop = "SIGTERM""#;
        let decoded: StopMechanismTest = toml::from_str(toml).expect("Failed to parse test TOML");
        assert_eq!(StopMechanism::Signal(SignalConfig::SIGTERM), decoded.stop);
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct CommandConfigTest {
        run: CommandConfig,
    }

    #[test]
    fn supports_whitespace_separated_command_lines() {
        let toml = r#"run = "/app/run-me.sh using these args""#;
        let decoded: CommandConfigTest = toml::from_str(toml).expect("Failed to parse test TOML");
        assert_eq!(
            CommandConfig {
                user: None,
                env_vars: Default::default(),
                program: String::from("/app/run-me.sh"),
                args: vec![
                    String::from("using"),
                    String::from("these"),
                    String::from("args"),
                ]
            },
            decoded.run
        );
    }

    #[test]
    fn supports_command_vectors() {
        let toml = r#"run = ["/app/run-me.sh", "using", "these", "args"]"#;
        let decoded: CommandConfigTest = toml::from_str(toml).expect("Failed to parse test TOML");
        assert_eq!(
            CommandConfig {
                user: None,
                env_vars: Default::default(),
                program: String::from("/app/run-me.sh"),
                args: vec![
                    String::from("using"),
                    String::from("these"),
                    String::from("args"),
                ]
            },
            decoded.run
        );
    }

    #[test]
    fn supports_detailed_whitespace_separated_command_lines() {
        let toml = r#"run = { command = "/app/run-me.sh using these args" }"#;
        let decoded: CommandConfigTest = toml::from_str(toml).expect("Failed to parse test TOML");
        assert_eq!(
            CommandConfig {
                user: None,
                env_vars: Default::default(),
                program: String::from("/app/run-me.sh"),
                args: vec![
                    String::from("using"),
                    String::from("these"),
                    String::from("args"),
                ]
            },
            decoded.run
        );

        let toml = r#"run = { user = "app", command = "/app/run-me.sh using these args" }"#;
        let decoded: CommandConfigTest = toml::from_str(toml).expect("Failed to parse test TOML");
        assert_eq!(
            CommandConfig {
                user: Some(String::from("app")),
                env_vars: Default::default(),
                program: String::from("/app/run-me.sh"),
                args: vec![
                    String::from("using"),
                    String::from("these"),
                    String::from("args"),
                ]
            },
            decoded.run
        );
    }

    #[test]
    fn supports_detailed_command_vectors() {
        let toml = r#"run = { command = ["/app/run-me.sh", "using", "these", "args"] }"#;
        let decoded: CommandConfigTest = toml::from_str(toml).expect("Failed to parse test TOML");
        assert_eq!(
            CommandConfig {
                user: None,
                env_vars: Default::default(),
                program: String::from("/app/run-me.sh"),
                args: vec![
                    String::from("using"),
                    String::from("these"),
                    String::from("args"),
                ]
            },
            decoded.run
        );

        let toml = r#"run = { user = "app", env-vars = ["USER", "HOME"], command = ["/app/run-me.sh", "using", "these", "args"] }"#;
        let decoded: CommandConfigTest = toml::from_str(toml).expect("Failed to parse test TOML");
        assert_eq!(
            CommandConfig {
                user: Some(String::from("app")),
                env_vars: HashSet::from(["USER".into(), "HOME".into()]),
                program: String::from("/app/run-me.sh"),
                args: vec![
                    String::from("using"),
                    String::from("these"),
                    String::from("args"),
                ]
            },
            decoded.run
        );
    }

    #[test]
    fn requires_command_in_detailed_command() {
        let toml = r#"run = { }"#;
        let error = toml::from_str::<CommandConfigTest>(toml).unwrap_err();
        assert_eq!("data did not match any variant of untagged enum CommandLineConfig for key `run` at line 1 column 1", error.to_string(),);

        let toml = r#"run = { user = "app" }"#;
        let error = toml::from_str::<CommandConfigTest>(toml).unwrap_err();
        assert_eq!("data did not match any variant of untagged enum CommandLineConfig for key `run` at line 1 column 1", error.to_string(),);
    }
}