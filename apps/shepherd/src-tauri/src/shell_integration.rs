use std::io;
use std::path::Path;

use tokio::sync::OnceCell;

const ZSH_WRAPPER: &str = r#"
if [ "${ZDOTDIR+x}" = x ]; then
    export SHEPHERD_USER_ZDOTDIR="$ZDOTDIR"
else
    unset SHEPHERD_USER_ZDOTDIR
fi
export ZDOTDIR="$1"
exec "$2"
"#;

const ZSH_ENV: &str = r#"
typeset -g __shepherd_rcdir=$ZDOTDIR
if (( ${+SHEPHERD_USER_ZDOTDIR} )); then
    ZDOTDIR=$SHEPHERD_USER_ZDOTDIR
else
    unset ZDOTDIR
fi
unset SHEPHERD_USER_ZDOTDIR
[[ ! -r ${ZDOTDIR:-$HOME}/.zshenv ]] || source "${ZDOTDIR:-$HOME}/.zshenv"
if [[ -o interactive && -o rcs ]]; then
    if (( ${+ZDOTDIR} )); then
        typeset -g __shepherd_user_zdotdir=$ZDOTDIR
    fi
    ZDOTDIR=$__shepherd_rcdir
fi
unset __shepherd_rcdir
"#;

const ZSH_RC: &str = r#"
if (( ${+__shepherd_user_zdotdir} )); then
    ZDOTDIR=$__shepherd_user_zdotdir
else
    unset ZDOTDIR
fi
unset __shepherd_user_zdotdir
[[ ! -r ${ZDOTDIR:-$HOME}/.zshrc ]] || source "${ZDOTDIR:-$HOME}/.zshrc"

__shepherd_preexec() {
    typeset -g __shepherd_command_started=1
    printf '\033]133;C\007'
}
__shepherd_precmd() {
    local command_status=$?
    if (( ${+__shepherd_command_started} )); then
        unset __shepherd_command_started
        printf '\033]133;D;%s\007' "$command_status"
    else
        printf '\033]133;A\007'
    fi
}
typeset -ga precmd_functions preexec_functions
precmd_functions=(__shepherd_precmd "${precmd_functions[@]}")
preexec_functions+=(__shepherd_preexec)
"#;

const BASH_RC: &str = r#"
[[ ! -r ~/.bashrc ]] || source ~/.bashrc

__shepherd_prompt() {
    local command_status=$?
    if [[ -n ${__shepherd_first_prompt-} ]]; then
        printf '\033]133;D;%s\007' "$command_status"
    else
        __shepherd_first_prompt=1
        printf '\033]133;A\007'
    fi
    return "$command_status"
}
if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a '* ]]; then
    PROMPT_COMMAND=(__shepherd_prompt "${PROMPT_COMMAND[@]}")
else
    PROMPT_COMMAND="__shepherd_prompt${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
fi
PS0=$'\033]133;C\007'${PS0-}
"#;

const FISH_INIT: &str = r#"
function __shepherd_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end
function __shepherd_postexec --on-event fish_postexec
    set -l command_status $status
    printf '\e]133;D;%s\a' $command_status
end
printf '\e]133;A\a'
"#;

#[derive(Default)]
pub struct ShellIntegration {
    startup_files: OnceCell<tempfile::TempDir>,
}

impl ShellIntegration {
    pub async fn launch(&self, shell: &str) -> io::Result<(String, Vec<String>)> {
        let basename = Path::new(shell).file_name().and_then(|name| name.to_str());
        let args = match basename {
            Some("zsh" | "bash") => {
                let directory = self
                    .startup_files
                    .get_or_try_init(|| async {
                        tokio::task::spawn_blocking(|| {
                            let directory = tempfile::Builder::new()
                                .prefix("shepherd-shell-")
                                .tempdir()?;
                            std::fs::write(directory.path().join(".zshenv"), ZSH_ENV)?;
                            std::fs::write(directory.path().join(".zshrc"), ZSH_RC)?;
                            std::fs::write(directory.path().join("bashrc"), BASH_RC)?;
                            Ok::<_, io::Error>(directory)
                        })
                        .await
                        .map_err(io::Error::other)?
                    })
                    .await?;
                if basename == Some("zsh") {
                    return Ok((
                        "/bin/sh".into(),
                        vec![
                            "-c".into(),
                            ZSH_WRAPPER.into(),
                            "shepherd-shell".into(),
                            directory.path().to_string_lossy().into_owned(),
                            shell.to_owned(),
                        ],
                    ));
                }
                vec![
                    "--rcfile".into(),
                    directory
                        .path()
                        .join("bashrc")
                        .to_string_lossy()
                        .into_owned(),
                ]
            }
            Some("fish") => vec!["--init-command".into(), FISH_INIT.into()],
            _ => Vec::new(),
        };
        Ok((shell.to_owned(), args))
    }
}
