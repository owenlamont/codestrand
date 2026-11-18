//! Options read from a `--config` TOML file, keyed by their flag names.
//!
//! Clap stays the one definition of every option: file values become flags spliced
//! in after the subcommand, so they meet the same parsers, defaults and constraints.

use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow, bail};
use clap::{
    Arg, ArgAction, ArgMatches, Command, CommandFactory, ValueHint,
    error::{ContextKind, ContextValue},
    parser::ValueSource,
};

/// Subcommands whose options a config file may set; one file serves all of them.
const CONFIGURABLE: [&str; 2] = ["preview", "export"];

/// Returns `arguments` with the `--config` file's options spliced in after the
/// subcommand, leaving out any option the command line sets itself.
pub(super) fn arguments(arguments: Vec<OsString>) -> anyhow::Result<Vec<OsString>> {
    let command = crate::Cli::command();
    // A malformed command line still records what precedes its first error, and the
    // strict parse that follows reports that error.
    let Ok(matches) = command
        .clone()
        .ignore_errors(true)
        .try_get_matches_from(&arguments)
    else {
        return Ok(arguments);
    };
    let Some((name, given)) = matches.subcommand() else {
        return Ok(arguments);
    };
    let Some(path) = given.try_get_one::<PathBuf>("config").ok().flatten() else {
        return Ok(arguments);
    };
    let subcommand = command
        .find_subcommand(name)
        .expect("the matched subcommand is defined");
    let flags = flags(&command, subcommand, given, path)
        .with_context(|| format!("config file {}", path.display()))?;
    let after_subcommand = arguments
        .iter()
        .skip(1)
        .position(|argument| argument == name)
        .map_or(arguments.len(), |at| at + 2);
    let mut arguments = arguments;
    arguments.splice(after_subcommand..after_subcommand, flags);
    Ok(arguments)
}

fn flags(
    command: &Command,
    subcommand: &Command,
    given: &ArgMatches,
    path: &Path,
) -> anyhow::Result<Vec<OsString>> {
    let text = crate::repositories::read_small(path, "config file")?;
    let table: toml::Table = toml::from_str(&text).context("parse")?;
    let base = path.parent().unwrap_or(Path::new("."));
    let on_command_line =
        |id: &str| given.value_source(id) == Some(ValueSource::CommandLine);
    // The repository list is one setting, so a repository flag replaces all of it.
    let list_given =
        on_command_line("repository") || on_command_line("repositories_file");
    let mut flags = Vec::new();
    if table.contains_key("repositories") {
        anyhow::ensure!(
            !table.contains_key("repositories-file"),
            "set `repositories` or `repositories-file`, not both"
        );
        toml::from_str::<crate::repositories::ConfigDocument>(&text)
            .context("parse")?;
        if !list_given {
            flags.push(flag("repositories-file", path.as_os_str()));
        }
    }
    for (key, value) in table.iter().filter(|(key, _)| *key != "repositories") {
        let arg = option(subcommand, key);
        let known = arg.or_else(|| {
            CONFIGURABLE
                .iter()
                .filter_map(|name| command.find_subcommand(name))
                .find_map(|other| option(other, key))
        });
        let known = known.ok_or_else(|| anyhow!("unknown key `{key}`"))?;
        // Validate keys for another subcommand too, so one file serves both.
        let values = values(known, key, value, base)?;
        if arg.is_some()
            && !on_command_line(known.get_id().as_str())
            && !(list_given
                && matches!(key.as_str(), "repository" | "repositories-file"))
        {
            flags.extend(values);
        }
    }
    Ok(flags)
}

/// The visible option `--key` of `command`, excluding `--config` itself.
fn option<'a>(command: &'a Command, key: &str) -> Option<&'a Arg> {
    command.get_arguments().find(|arg| {
        arg.get_long() == Some(key)
            && key != "config"
            && !arg.is_hide_set()
            && matches!(
                arg.get_action(),
                ArgAction::Set | ArgAction::Append | ArgAction::SetTrue
            )
    })
}

fn values(
    arg: &Arg,
    key: &str,
    value: &toml::Value,
    base: &Path,
) -> anyhow::Result<Vec<OsString>> {
    match (arg.get_action(), value) {
        (ArgAction::SetTrue, toml::Value::Boolean(on)) => {
            Ok(on.then(|| format!("--{key}").into()).into_iter().collect())
        }
        (ArgAction::SetTrue, _) => bail!("key `{key}` must be true or false"),
        (ArgAction::Append, toml::Value::Array(items)) => items
            .iter()
            .map(|item| setting(arg, key, item, base))
            .collect(),
        _ => Ok(vec![setting(arg, key, value, base)?]),
    }
}

fn setting(
    arg: &Arg,
    key: &str,
    value: &toml::Value,
    base: &Path,
) -> anyhow::Result<OsString> {
    let text = match value {
        toml::Value::String(text) => text.clone(),
        toml::Value::Integer(number) => number.to_string(),
        toml::Value::Float(number) => number.to_string(),
        toml::Value::Boolean(on) => on.to_string(),
        toml::Value::Datetime(date) => date.to_string(),
        toml::Value::Array(_) | toml::Value::Table(_) => {
            bail!("key `{key}` takes a single value")
        }
    };
    check(arg, key, &text)?;
    // Paths resolve beside the file, as repository paths in it do; a command name
    // is looked up on PATH as it would be from the command line.
    let text = if arg.get_value_parser().type_id()
        == clap::value_parser!(PathBuf).type_id()
        && arg.get_value_hint() != ValueHint::CommandName
    {
        base.join(text).into_os_string()
    } else {
        text.into()
    };
    Ok(flag(key, &text))
}

/// Runs `arg`'s own value parser on `text`, naming the key on failure.
fn check(arg: &Arg, key: &str, text: &str) -> anyhow::Result<()> {
    let probe = Command::new("config").no_binary_name(true).arg(
        Arg::new(arg.get_id().clone())
            .long("value")
            .value_parser(arg.get_value_parser().clone()),
    );
    let Err(error) = probe.try_get_matches_from([format!("--value={text}")]) else {
        return Ok(());
    };
    let reason = std::error::Error::source(&error)
        .map(ToString::to_string)
        .or_else(|| match error.get(ContextKind::ValidValue) {
            Some(ContextValue::Strings(valid)) => {
                Some(format!("expected one of {}", valid.join(", ")))
            }
            _ => None,
        })
        .unwrap_or_else(|| error.kind().to_string());
    bail!("key `{key}`: invalid value '{text}': {reason}")
}

fn flag(key: &str, value: &OsStr) -> OsString {
    let mut flag = OsString::from(format!("--{key}="));
    flag.push(value);
    flag
}

#[cfg(test)]
mod tests;
