//! Whether Windows has already been asked about letting anything in.
//!
//! This is the failure with no symptom. The gateway binds `0.0.0.0` and reports
//! success — binding is not filtered — the QR code appears, the camera reads it,
//! and the phone's browser then sits on a white page until it times out. Nothing
//! on the desktop says anything, because from this process's side nothing
//! happened: the SYN was dropped by the filter before any code here could see
//! it. Every user who hits it concludes the feature is broken.
//!
//! What Windows actually does on a first listen is put up its own dialog —
//! "Windows Defender Firewall has blocked some features of this app" — with
//! Allow and Cancel. Cancel writes a *block* rule, and it is never offered
//! again. So the two states worth telling apart are "Windows has an opinion
//! about this program already" and "it is about to ask", which is what
//! [`asked`] answers.
//!
//! ## Why the answer is not "allowed" or "blocked"
//!
//! Because reading that out of the firewall means parsing `netsh`'s output, and
//! `netsh`'s field names and its values are both localised — `Action: Allow` is
//! `操作: 允许` on the machine this was written on. A parser built on those
//! would work here and quietly stop working on an English install, which is the
//! worst way for a check like this to fail.
//!
//! The one thing in that output that is the same in every language is the path
//! of the program a rule names, because it is a path. So that is what is looked
//! for, and the answer is the honest one: whether this program is in there at
//! all.
//!
//! The rest of the guidance does not come from here. The card watches whether
//! anything has actually connected — see `crate::remote::card` — which is a
//! direct observation of the symptom, catches third-party firewalls that `netsh`
//! knows nothing about, and needs no parsing at all.

/// What is known about this program's standing with the firewall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Firewall {
    /// A rule names this program. Windows has been asked and will not ask
    /// again — which also means that if the answer was Cancel, nothing is
    /// getting in until the user changes it themselves.
    Known,
    /// No rule names it. The first connection will raise Windows' own dialog,
    /// which is worth warning about *before* the user walks off with the phone.
    Unasked,
    /// Nothing could be found out: not Windows, or `netsh` did not answer.
    Unknown,
}

/// Ask the firewall about this program.
///
/// Slow — a second or two, and a megabyte of text — so it is called once when
/// the gateway starts, from a worker thread, and the answer is kept. It is
/// never on the path of a request.
#[cfg(windows)]
pub fn asked() -> Firewall {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let Ok(program) = std::env::current_exe() else {
        return Firewall::Unknown;
    };

    let mut command = Command::new("netsh");
    command
        .args([
            "advfirewall",
            "firewall",
            "show",
            "rule",
            "name=all",
            "dir=in",
            "verbose",
        ])
        .creation_flags(crate::server::CREATE_NO_WINDOW);

    let Ok(output) = command.output() else {
        return Firewall::Unknown;
    };
    if !output.status.success() {
        return Firewall::Unknown;
    }

    // Lossy rather than strict: the output is in the console code page, and the
    // localised field names in it are exactly the part nothing here reads. A
    // path of ASCII survives the conversion either way, and a path that is not
    // ASCII survives it too — the replacement characters land on the bytes that
    // did not decode, and a rule whose program path did not decode is one this
    // cannot match, which is `Unasked` rather than a wrong answer.
    let rules = String::from_utf8_lossy(&output.stdout);
    match names(&rules, &program.to_string_lossy()) {
        true => Firewall::Known,
        false => Firewall::Unasked,
    }
}

#[cfg(not(windows))]
pub fn asked() -> Firewall {
    // macOS has an application firewall of its own and Linux distributions have
    // several, none of which is asked like this. The card's silence watch covers
    // all of them, and covers Windows too.
    Firewall::Unknown
}

/// Whether a `netsh` listing mentions this program.
///
/// Case-insensitive, because a Windows path is, and the case in the rule is
/// whatever the installer wrote rather than whatever `current_exe` reports.
///
/// Its own function, and the only part of the check with a rule in it, so that
/// a test can hold on to the shape without running `netsh`.
#[cfg_attr(not(windows), allow(dead_code))]
fn names(rules: &str, program: &str) -> bool {
    rules
        .to_ascii_lowercase()
        .contains(&program.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `netsh` prints, in both the language it was written in and the
    /// one it is read in. The field names differ; the path does not.
    #[test]
    fn a_rule_is_found_by_its_program_path() {
        let english = "\
Rule Name:                            dsh-desktop
----------------------------------------------------------------------
Enabled:                              Yes
Direction:                            In
Program:                              C:\\Program Files\\dsh-desktop\\dsh-desktop.exe
Action:                               Allow
";
        let chinese = "\
规则名称:                             dsh-desktop
----------------------------------------------------------------------
已启用:                               是
方向:                                 入
程序:                                 C:\\Program Files\\dsh-desktop\\dsh-desktop.exe
操作:                                 允许
";

        for listing in [english, chinese] {
            assert!(names(
                listing,
                "C:\\Program Files\\dsh-desktop\\dsh-desktop.exe"
            ));
        }
    }

    /// Rules are written with whatever case the installer used, and Windows
    /// does not care about it either.
    #[test]
    fn the_case_of_the_path_does_not_decide_it() {
        let listing = "Program: C:\\Program Files\\DSH-Desktop\\DSH-Desktop.exe";
        assert!(names(
            listing,
            "c:\\program files\\dsh-desktop\\dsh-desktop.exe"
        ));
    }

    /// A machine with rules for everything else and none for this one is the
    /// case the warning exists for.
    #[test]
    fn another_programs_rule_is_not_this_ones() {
        let listing = "Program: C:\\Program Files\\nodejs\\node.exe";
        assert!(!names(
            listing,
            "C:\\Program Files\\dsh-desktop\\dsh-desktop.exe"
        ));
    }

    #[test]
    fn an_empty_listing_names_nothing() {
        assert!(!names("", "C:\\dsh-desktop.exe"));
    }
}
