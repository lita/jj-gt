//! Line-based interactive prompts, in the spirit of `git add -p`.
//!
//! Questions go to stdout and answers are read from stdin one line at a time.
//! Reading stdin (rather than `/dev/tty`) keeps the prompts scriptable: a
//! piped stdin works exactly like a terminal, and hitting EOF is a clean
//! "this command needs interactive input" error instead of a hang.

use std::io::{BufRead, BufReader, IsTerminal, Stdin, Write};

use anyhow::{Result, bail};
use owo_colors::OwoColorize;

pub struct Prompt {
    stdin: BufReader<Stdin>,
    is_terminal: bool,
}

impl Prompt {
    pub fn new() -> Self {
        let stdin = std::io::stdin();
        let is_terminal = stdin.is_terminal();
        Prompt {
            stdin: BufReader::new(stdin),
            is_terminal,
        }
    }

    /// True when a human is on the other end of stdin.
    pub fn is_terminal(&self) -> bool {
        self.is_terminal
    }

    /// Print `question` (no trailing newline) and read one trimmed line.
    pub fn line(&mut self, question: &str) -> Result<String> {
        print!("{question}");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        let n = self.stdin.read_line(&mut answer)?;
        if n == 0 {
            println!();
            bail!("interactive input required — run this command in a terminal");
        }
        let answer = answer.trim().to_owned();
        // Echo scripted answers so a piped transcript still reads naturally.
        if !self.is_terminal {
            println!("{answer}");
        }
        Ok(answer)
    }

    /// Free-text input; an empty answer accepts `default`.
    pub fn input(&mut self, question: &str, default: &str) -> Result<String> {
        let answer = self.line(&format!("{question} [{}]: ", default.dimmed()))?;
        Ok(if answer.is_empty() {
            default.to_owned()
        } else {
            answer
        })
    }

    /// Yes/no question; an empty answer picks `default`.
    pub fn confirm(&mut self, question: &str, default: bool) -> Result<bool> {
        let hint = if default { "Y/n" } else { "y/N" };
        loop {
            let answer = self.line(&format!("{question} [{hint}] "))?;
            match answer.to_ascii_lowercase().as_str() {
                "" => return Ok(default),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => println!("Please answer y or n."),
            }
        }
    }

    /// Pick one of `options` by its key letter; re-asks on anything else.
    pub fn choose(&mut self, question: &str, options: &[(char, &str)]) -> Result<char> {
        println!("{question}");
        for (key, label) in options {
            println!("  {}  {label}", format!("[{key}]").bold());
        }
        let keys: String = options.iter().map(|(key, _)| *key).collect();
        loop {
            let answer = self.line(&format!("Choice [{keys}]: "))?;
            let mut chars = answer.chars();
            if let (Some(ch), None) = (chars.next(), chars.next())
                && let Some((key, _)) = options
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(&ch))
            {
                return Ok(*key);
            }
            println!("Please answer one of: {keys}");
        }
    }

    /// Parse a comma/space-separated list of 1-based indices in `1..=max`.
    /// Returns them sorted and deduplicated; an empty answer is an empty set.
    pub fn indices(&mut self, question: &str, max: usize) -> Result<Vec<usize>> {
        loop {
            let answer = self.line(question)?;
            let mut picked = Vec::new();
            let mut ok = true;
            for token in answer.split(|c: char| c == ',' || c.is_whitespace()) {
                if token.is_empty() {
                    continue;
                }
                match token.parse::<usize>() {
                    Ok(n) if (1..=max).contains(&n) => picked.push(n),
                    _ => {
                        println!("{token:?} is not a number between 1 and {max}.");
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                picked.sort_unstable();
                picked.dedup();
                return Ok(picked);
            }
        }
    }
}
