use std::io;
use std::path::Path;

use serde::Deserialize;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileAction {
    Edit,
    Reveal,
}

impl FileAction {
    pub(crate) fn error_context(self) -> &'static str {
        match self {
            Self::Edit => "Could not open the file in the external editor",
            Self::Reveal => "Could not reveal the file in the file manager",
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn perform(path: &Path, action: FileAction) -> io::Result<()> {
    let output = macos_command(path, action).output()?;
    if output.status.success() {
        return Ok(());
    }
    let error = String::from_utf8_lossy(&output.stderr);
    let message = error.trim();
    Err(io::Error::other(if message.is_empty() {
        format!("The desktop opener returned {}.", output.status)
    } else {
        message.to_string()
    }))
}

#[cfg(target_os = "macos")]
fn macos_command(path: &Path, action: FileAction) -> std::process::Command {
    let mut command = std::process::Command::new("/usr/bin/open");
    command
        .arg(match action {
            FileAction::Edit => "-t",
            FileAction::Reveal => "-R",
        })
        .arg(path);
    command
}

#[cfg(target_os = "linux")]
pub(crate) fn perform(path: &Path, action: FileAction) -> io::Result<()> {
    use gio::prelude::*;

    let file = gio::File::for_path(path);
    match action {
        FileAction::Edit => {
            let editor = gio::AppInfo::default_for_type("text/plain", false).ok_or_else(|| {
                io::Error::other("No desktop text editor is associated with text/plain.")
            })?;
            editor
                .launch(&[file], gio::AppLaunchContext::NONE)
                .map_err(io::Error::other)
        }
        FileAction::Reveal => {
            let uri = file.uri();
            let parameters = (vec![uri.as_str()], "").to_variant();
            let shown =
                gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).and_then(|bus| {
                    bus.call_sync(
                        Some("org.freedesktop.FileManager1"),
                        "/org/freedesktop/FileManager1",
                        "org.freedesktop.FileManager1",
                        "ShowItems",
                        Some(&parameters),
                        None,
                        gio::DBusCallFlags::NONE,
                        1_500,
                        gio::Cancellable::NONE,
                    )
                });
            if let Err(reveal) = shown {
                let parent = file
                    .parent()
                    .ok_or_else(|| io::Error::other("The file has no containing folder."))?;
                gio::AppInfo::launch_default_for_uri(
                    parent.uri().as_str(),
                    gio::AppLaunchContext::NONE,
                )
                .map_err(|open| {
                    io::Error::other(format!(
                        "Could not select the file ({reveal}) or open its folder ({open})."
                    ))
                })?;
            }
            Ok(())
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn perform(_path: &Path, _action: FileAction) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "External file actions are unavailable on this platform.",
    ))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn passes_a_file_with_spaces_and_shell_characters_as_one_argument() {
        let path = Path::new("/tmp/project/report $(draft).md");
        for (action, flag) in [(FileAction::Edit, "-t"), (FileAction::Reveal, "-R")] {
            let command = macos_command(path, action);
            assert_eq!(command.get_program(), "/usr/bin/open");
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                [OsStr::new(flag), path.as_os_str()]
            );
        }
    }
}
