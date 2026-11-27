# The command line

`lightcraft-cli` is the headless LightCraft: `run`, `mcp`, `render`, `snapshot`, `merge`,
`synth-merge`, `denoise-serve`, `commands`, `controls`, `calibrate` and `bench-cull`. **Follows
[RFC-0018](https://github.com/storytold/craft-contrib/blob/main/rfcs/0018-cli-conventions.md)**
(command-line conventions), except for the rules listed at the end.

`--help` is the reference for every option, `lightcraft-cli <command> --help` for one command; this
page only says what `--help` can't. The Linux and FreeBSD packages install man pages
(`man lightcraft-cli`, `man lightcraft-cli-render`, …) and bash, zsh and fish completions, written
from the same definition (`cargo run -p lightcraft-cli --example man-and-completions -- DIR`).
`--version` and `-V` work on every command; there is no `version` command.

## Exit codes and streams

| Code | Meaning |
|---|---|
| 0 | Success. Also `--help` and `--version`. |
| 1 | The command failed: a file could not be read, the engine refused, a `run` command or a `snapshot` script request failed. `lightcraft-cli: <message>` on stderr. |
| 2 | Usage error: the command line was refused before any work started, or `lightcraft-cli` was run without a command. `error: <message>` on stderr, or the usage text. |

Results go to stdout (`run` prints one JSON line per command; `mcp` writes only the protocol);
messages, warnings, progress and the log go to stderr.

## Strict options

These are usage errors (exit 2), checked before anything is imported, opened or rendered:

- an unknown option after any command, or a command word that does not exist;
- an option given twice where the second would silently replace the first (`-o`, `--script`,
  `--library`, …); `--import`, `--set` and `--opt` repeat;
- a value of the wrong type or out of range (`--quality 0`, `--boundary-warp 120`,
  `--run-on tpu`, `--set light.exposure=inf`);
- a missing required argument (`render` needs `-o`, `merge` needs files);
- options that cannot be combined (`run --connect` with `--demo`, `--library` or `--import`; `mcp
  --connect` with files, `--demo`, `--library` or `--headless`);
- a value that is another option: write a value that starts with `-` as `--option=-value`;
  `--` ends the options.

`--connect` takes an optional address, as before: `--connect ADDR` and `--connect PORT` (a port on
127.0.0.1) are understood, and a word that is neither (a command id such as `ui.inspect`) is not
taken for the address.

## Environment

| Variable | Used by | Notes |
|---|---|---|
| `LIGHTCRAFT_LOG`, `RUST_LOG` | every command | Log level on stderr (read by the engine's logging). |
| `LIGHTCRAFT_DENOISE_TOKEN` | `denoise-serve` | The shared token. The variable *named* by `--token-env VAR` is read, so the secret is never on a command line. |
| `LIGHTCRAFT_DENOISE_MODELS`, `LIGHTCRAFT_CAMERA_PROFILES`, `LIGHTCRAFT_GPU` | the engine | Read by the engine for folders and the compute policy. |

No variable sets a destructive option or grants file access.

## Where LightCraft follows RFC-0018 only in part

- Rule 1.7: `LIGHTCRAFT_GPU`, `LIGHTCRAFT_DENOISE_MODELS`, `LIGHTCRAFT_CAMERA_PROFILES` and
  `LIGHTCRAFT_DHAT_FILE` are read by the engine or a development build, not declared on a flag with
  clap's `env`; moving them is a follow-up.
- Rule 2.2: handlers still return the engine's messages as text inside `CliError::Failed`; typed
  causes for the engine's errors are a follow-up. The exit code is decided by the error type.
