//! Colourises tracing logs of form `TIMESTAMP LOG_LEVEL SOURCE: LOG_MESSAGE` eg.
//!
//! 2025-08-28T04:57:18.797136Z INFO  crate::path::file: I am the log message
//!
//! We assume that the TIMESTAMP will be of constant length within a set of logs,
//! that the LOG_LEVEL will of length 5, padded with spaces at the end and that the
//! SOURCE and LOG_MESSAGE will vary in length. As such, we parse the first log that
//! comes in fully, before reusing the indices for the TIMESTAMP, LOG_LEVEL and start
//! of SOURCE and simply parsing the rest of the string from there.
//!
use std::fs::{self, File};
use std::io::{self, BufRead, Write};
use std::process::{Child, Command, Stdio, exit};

use clap::{Parser, command};

/// Recolour tracing logs and view them in less. Supports piping of input and output
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// The log file to parse
    file: Option<String>,

    /// Output directly to stdout for piping rather than opening less
    #[arg(short = 'P', long = "pipe")]
    pipe: bool,

    /// Arguments to pass directly to less (use -- to separate)
    #[arg(trailing_var_arg = true)]
    less_args: Vec<String>,
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    let reader = if let Some(file) = args.file {
        let file = File::open(&file)?;
        InputSource::File(io::BufReader::new(file))
    } else {
        let is_a_tty = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
        if is_a_tty {
            eprintln!("Missing filename");
            exit(1)
        }
        InputSource::Pipe(io::stdin().lock())
    };

    let mut child: Option<Child> = None;
    let mut write_destination = if args.pipe {
        WriteDestination::Stdout(io::stdout().lock())
    } else {
        let mut less_process = Command::new("less")
            .arg("-R")
            .args(&args.less_args)
            .stdin(Stdio::piped())
            .spawn()
            .expect("Failed to start less");

        let less_stdin = less_process
            .stdin
            .take()
            .expect("Failed to open less stdin");

        child = Some(less_process);
        WriteDestination::Less(less_stdin)
    };

    let mut general_format = None;
    for line in reader.lines() {
        let l = line?;
        let new_line = if let Some(format) = general_format {
            if let Some(full_format) = parse_line_path_from_general_format(&l, format) {
                colorize_line(&l, full_format)
            } else {
                format!("FAILED TO PARSE LINE: {}", l)
            }
        } else if let Some(full_format) = parse_line_from_scratch(&l) {
            general_format = Some(GeneralLineFormat {
                tz_start: full_format.tz_start,
                tz_end: full_format.tz_end,
                level_start: full_format.level_start,
                level_end: full_format.level_end,
                path_start: full_format.path_start,
            });
            colorize_line(&l, full_format)
        } else {
            format!("FAILED TO PARSE LINE: {}", l)
        };
        writeln!(write_destination, "{}", new_line)?;
    }

    if let Some(mut child) = child {
        drop(write_destination);
        child.wait()?;
    }

    Ok(())
}

enum InputSource {
    File(io::BufReader<fs::File>),
    Pipe(io::StdinLock<'static>),
}

impl std::io::Read for InputSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::File(f) => f.read(buf),
            Self::Pipe(p) => p.read(buf),
        }
    }
}

impl std::io::BufRead for InputSource {
    fn consume(&mut self, amount: usize) {
        match self {
            Self::File(f) => f.consume(amount),
            Self::Pipe(p) => p.consume(amount),
        }
    }
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        match self {
            Self::File(f) => f.fill_buf(),
            Self::Pipe(p) => p.fill_buf(),
        }
    }
}

enum WriteDestination {
    Stdout(io::StdoutLock<'static>),
    Less(std::process::ChildStdin),
}

impl std::io::Write for WriteDestination {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Less(w) => w.write(buf),
            Self::Stdout(w) => w.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Less(w) => w.flush(),
            Self::Stdout(w) => w.flush(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum LogType {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogType {
    pub fn as_colour_str(&self) -> &'static str {
        match self {
            Self::Error => "\x1b[91m", // Red
            Self::Warn => "\x1b[93m",  // Yellow
            Self::Info => "\x1b[92m",  // Green
            Self::Debug => "\x1b[94m", // Blue
            Self::Trace => "\x1b[95m", // Purple
        }
    }
}

#[derive(Clone, Copy)]
struct GeneralLineFormat {
    tz_start: usize,
    tz_end: usize,
    level_start: usize,
    level_end: usize,
    path_start: usize,
}

#[derive(Clone, Copy, Debug)]
struct LineFormat {
    log_type: LogType,
    tz_start: usize,
    tz_end: usize,
    level_start: usize,
    level_end: usize,
    path_start: usize,
    path_end: usize,
}

/// Parse the line to obtain the full format.
///
/// Returns None if it fails to parse. Returning
/// Some does not guarantee a correct parse.
fn parse_line_from_scratch(line: &str) -> Option<LineFormat> {
    if line.len() < 4 {
        return None;
    }
    let mut space_indices = Vec::with_capacity(3);
    let mut count = 0;
    let mut prev_was_space = false;

    for (i, c) in line.char_indices() {
        if c == ' ' {
            if !prev_was_space {
                space_indices.push(i);
                count += 1;
                if count > 2 {
                    break;
                }
            }

            prev_was_space = true
        } else {
            prev_was_space = false
        }
    }

    let log_type = match line[(space_indices[0] + 1)..(space_indices[1])]
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "error" => LogType::Error,
        "warn" => LogType::Warn,
        "info" => LogType::Info,
        "debug" => LogType::Debug,
        "trace" => LogType::Trace,
        _ => return None,
    };

    Some(LineFormat {
        log_type,
        tz_start: 0,
        tz_end: space_indices[0] + 1,
        level_start: space_indices[0] + 1,
        level_end: space_indices[1] + 1,
        path_start: space_indices[1] + 1,
        path_end: space_indices[2] + 1,
    })
}

/// Given a general format, parse the log type and path end location to create a full LineFormat
fn parse_line_path_from_general_format(
    line: &str,
    general_format: GeneralLineFormat,
) -> Option<LineFormat> {
    let log_type = match line[general_format.level_start..general_format.level_end]
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "error" => LogType::Error,
        "warn" => LogType::Warn,
        "info" => LogType::Info,
        "debug" => LogType::Debug,
        "trace" => LogType::Trace,
        _ => return None,
    };

    let path_end = line[general_format.path_start..].find(' ')? + general_format.path_start;
    Some(LineFormat {
        log_type,
        tz_start: general_format.tz_start,
        tz_end: general_format.tz_end,
        level_start: general_format.level_start,
        level_end: general_format.level_end,
        path_start: general_format.path_start,
        path_end,
    })
}

fn colorize_line(line: &str, line_format: LineFormat) -> String {
    let mut new_line = String::with_capacity(line.len() + 24);
    let grey = "\x1b[90m";
    new_line.push_str(grey);
    new_line.push_str(&line[line_format.tz_start..line_format.tz_end]);
    new_line.push_str(line_format.log_type.as_colour_str());
    new_line.push_str(&line[line_format.level_start..line_format.level_end]);
    new_line.push_str(grey);
    new_line.push_str(&line[line_format.path_start..line_format.path_end]);
    new_line.push_str("\x1b[0m"); // Clear colour formatting for rest of string
    new_line.push_str(&line[line_format.path_end..]);

    new_line
}
