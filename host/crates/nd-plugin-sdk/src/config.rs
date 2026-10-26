// SPDX-License-Identifier: GPL-3.0-or-later
use crate::Error;

/// Connection parameters the agent passes to a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub port: u16,
    pub token: String,
    pub plugin_id: String,
}

impl Config {
    /// Read from the process arguments, falling back to `NOTIDECK_PORT/TOKEN/PLUGIN_ID`.
    pub fn from_process() -> Result<Config, Error> {
        Config::parse(std::env::args().skip(1), |k| std::env::var(k).ok())
    }

    /// Parse `--port <n> --token <t> --plugin-id <id>` (also `--flag=value`); anything missing comes
    /// from the `NOTIDECK_*` environment variables via `env`. Unknown arguments are ignored.
    pub fn parse<I, F>(args: I, env: F) -> Result<Config, Error>
    where
        I: IntoIterator<Item = String>,
        F: Fn(&str) -> Option<String>,
    {
        let (mut port, mut token, mut id) = (None, None, None);
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            let (flag, inline) = match a.split_once('=') {
                Some((f, v)) => (f.to_string(), Some(v.to_string())),
                None => (a, None),
            };
            let slot = match flag.as_str() {
                "--port" => &mut port,
                "--token" => &mut token,
                "--plugin-id" => &mut id,
                _ => continue,
            };
            *slot = inline.or_else(|| it.next());
        }
        let port = port
            .or_else(|| env("NOTIDECK_PORT"))
            .ok_or_else(|| Error::Config("missing --port / NOTIDECK_PORT".into()))?;
        let port: u16 = port.trim().parse().map_err(|_| Error::Config(format!("bad port {port:?}")))?;
        let token = token
            .or_else(|| env("NOTIDECK_TOKEN"))
            .ok_or_else(|| Error::Config("missing --token / NOTIDECK_TOKEN".into()))?;
        let plugin_id = id
            .or_else(|| env("NOTIDECK_PLUGIN_ID"))
            .ok_or_else(|| Error::Config("missing --plugin-id / NOTIDECK_PLUGIN_ID".into()))?;
        Ok(Config { port, token, plugin_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn args() {
        let c = Config::parse(s(&["--port", "123", "--token", "t", "--plugin-id", "a.b"]), |_| None).unwrap();
        assert_eq!(c, Config { port: 123, token: "t".into(), plugin_id: "a.b".into() });
    }

    #[test]
    fn equals_form_and_unknown() {
        let c = Config::parse(s(&["--x", "--port=9", "--token=t", "--plugin-id=i"]), |_| None).unwrap();
        assert_eq!(c.port, 9);
    }

    #[test]
    fn env_fallback_and_arg_priority() {
        let env = |k: &str| match k {
            "NOTIDECK_PORT" => Some("1".into()),
            "NOTIDECK_TOKEN" => Some("et".into()),
            "NOTIDECK_PLUGIN_ID" => Some("ei".into()),
            _ => None,
        };
        let c = Config::parse(s(&["--port", "2"]), env).unwrap();
        assert_eq!((c.port, c.token.as_str(), c.plugin_id.as_str()), (2, "et", "ei"));
    }

    #[test]
    fn missing_and_bad() {
        assert!(Config::parse(s(&[]), |_| None).is_err());
        assert!(Config::parse(s(&["--port", "x", "--token", "t", "--plugin-id", "i"]), |_| None).is_err());
    }
}
