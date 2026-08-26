//! Reading spot's command line.
//!
//! Four words and a link, so this is hand-rolled rather than a dependency.
//! Every branch except [`Invocation::Run`] prints and exits, and all of them
//! are answered before the Windows Terminal bounce in `main` — a flag that
//! opened a new window would print into one that closes with the process.

use crate::link::{self, Link, ParseError};

pub const REGISTER: &str = "--register-protocol";
pub const UNREGISTER: &str = "--unregister-protocol";

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Start normally, following a link if one came with it.
    Run(Option<Link>),
    /// Claim the `spotify:` and `spot:` schemes. `force` answers for the user
    /// when another app already holds `spotify:`; without it the claim is
    /// refused, so a flag copied out of the README cannot displace an
    /// installed app before its owner has read what it does.
    Register {
        force: bool,
    },
    Unregister,
    Help,
    Version,
    /// Nothing spot understands. Carries the line to print.
    Rejected(String),
}

/// ASCII only: this may reach a legacy console codepage.
pub const HELP: &str = "\
spot - a standalone Spotify player for the terminal

USAGE:
  spot                          start spot
  spot <link>                   open a Spotify link, in the running spot if
                                there is one
  spot --register-protocol      open Spotify links in spot from now on
  spot --unregister-protocol    give the Spotify links back
  spot --help, --version

A link is either spelling: spotify:album:<id> or
https://open.spotify.com/album/<id>.

--register-protocol changes only your own account, needs no administrator, and
is undone by --unregister-protocol or from spot's Home screen. It refuses to
displace an app that already opens Spotify links unless you add --force.";

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Invocation {
    let args: Vec<String> = args.into_iter().collect();
    let flags: Vec<&str> = args.iter().map(String::as_str).collect();

    if flags.iter().any(|a| *a == "--help" || *a == "-h") {
        return Invocation::Help;
    }
    if flags.iter().any(|a| *a == "--version" || *a == "-V") {
        return Invocation::Version;
    }
    if flags.contains(&UNREGISTER) {
        return Invocation::Unregister;
    }
    if flags.contains(&REGISTER) {
        return Invocation::Register {
            force: flags.contains(&"--force"),
        };
    }

    match flags.as_slice() {
        [] => Invocation::Run(None),
        [one] => match link::parse(one) {
            Ok(target) => Invocation::Run(Some(target)),
            Err(ParseError::Unsupported(what)) => {
                Invocation::Rejected(format!("spot does not play {what}."))
            }
            // A leading dash reads as a mistyped flag rather than a mistyped
            // link, and saying so is more use than repeating the link rules.
            Err(ParseError::NotALink) if one.starts_with('-') => {
                Invocation::Rejected(format!("unknown option {one}. Try --help."))
            }
            Err(ParseError::NotALink) => {
                Invocation::Rejected(format!("{one} is not a Spotify link. Try --help."))
            }
        },
        _ => Invocation::Rejected("spot takes one link at a time. Try --help.".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "4uLU6hMCjMI75M1A2tKUQC";

    fn parse_of(args: &[&str]) -> Invocation {
        parse(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn nothing_runs_normally() {
        assert_eq!(parse_of(&[]), Invocation::Run(None));
    }

    #[test]
    fn a_link_rides_along() {
        assert_eq!(
            parse_of(&[&format!("https://open.spotify.com/album/{ID}")]),
            Invocation::Run(Some(Link::Album(ID.into())))
        );
    }

    #[test]
    fn reads_the_protocol_flags() {
        assert_eq!(parse_of(&[REGISTER]), Invocation::Register { force: false });
        assert_eq!(
            parse_of(&[REGISTER, "--force"]),
            Invocation::Register { force: true }
        );
        assert_eq!(parse_of(&[UNREGISTER]), Invocation::Unregister);
    }

    #[test]
    fn help_and_version_win_over_everything() {
        assert_eq!(parse_of(&["--help"]), Invocation::Help);
        assert_eq!(parse_of(&[REGISTER, "--help"]), Invocation::Help);
        assert_eq!(parse_of(&["-V"]), Invocation::Version);
    }

    #[test]
    fn giving_the_schemes_back_wins_over_claiming_them() {
        assert_eq!(parse_of(&[REGISTER, UNREGISTER]), Invocation::Unregister);
    }

    #[test]
    fn says_what_is_wrong() {
        let Invocation::Rejected(why) = parse_of(&["--nonsense"]) else {
            panic!("a mistyped flag is rejected")
        };
        assert!(why.contains("unknown option"), "{why}");

        let Invocation::Rejected(why) = parse_of(&[&format!("spotify:episode:{ID}")]) else {
            panic!("a podcast is rejected")
        };
        assert!(why.contains("podcasts"), "{why}");

        let Invocation::Rejected(why) = parse_of(&["one", "two"]) else {
            panic!("two links are rejected")
        };
        assert!(why.contains("one link at a time"), "{why}");
    }
}
