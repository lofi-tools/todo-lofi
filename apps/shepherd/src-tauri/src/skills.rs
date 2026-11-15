use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shep_core::Agent;

#[derive(Clone, Debug, Serialize)]
pub struct AgentSkill {
    id: String,
    name: String,
    description: String,
    path: String,
}

#[derive(Deserialize)]
struct SkillMetadata {
    name: Option<String>,
    description: Option<String>,
    #[serde(rename = "user-invocable")]
    user_invocable: Option<bool>,
}

pub fn discover(
    agent: Agent,
    directory: &Path,
    home: &Path,
    codex_home: &Path,
    config_home: &Path,
) -> Result<Vec<AgentSkill>, String> {
    let mut roots = Vec::new();
    match agent {
        Agent::Codex => {
            roots.push(directory.join(".agents/skills"));
            roots.push(directory.join(".codex/skills"));
            roots.push(home.join(".agents/skills"));
            roots.push(codex_home.join("skills"));
            roots.push(codex_home.join("skills/.system"));
        }
        Agent::Claude => {
            roots.push(directory.join(".claude/skills"));
            roots.push(home.join(".claude/skills"));
        }
        Agent::OpenCode => {
            for name in [".opencode/skills", ".claude/skills", ".agents/skills"] {
                roots.push(directory.join(name));
            }
            roots.push(config_home.join("opencode/skills"));
            roots.push(home.join(".claude/skills"));
            roots.push(home.join(".agents/skills"));
        }
        Agent::Pi => {
            roots.push(directory.join(".pi/skills"));
            roots.push(directory.join(".agents/skills"));
            roots.push(home.join(".pi/agent/skills"));
            roots.push(home.join(".agents/skills"));
        }
        Agent::Freebuff => return Ok(Vec::new()),
    }
    let mut skills = BTreeMap::new();
    let mut visited = HashSet::new();
    for root in roots {
        scan(&root, 0, agent, &mut visited, &mut skills)?;
    }
    Ok(skills.into_values().collect())
}

fn scan(
    path: &Path,
    depth: usize,
    agent: Agent,
    visited: &mut HashSet<PathBuf>,
    skills: &mut BTreeMap<String, AgentSkill>,
) -> Result<(), String> {
    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Could not inspect {}: {error}", path.display())),
    };
    if !visited.insert(canonical) || depth > 6 {
        return Ok(());
    }
    let skill_file = path.join("SKILL.md");
    if skill_file.is_file() {
        return read_skill(&skill_file, agent, skills);
    }
    let entries = std::fs::read_dir(path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let mut entries = entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') || name == "node_modules" {
            continue;
        }
        let child = entry.path();
        if child.is_dir() {
            scan(&child, depth + 1, agent, visited, skills)?;
        } else if depth == 0
            && matches!(agent, Agent::OpenCode | Agent::Pi)
            && child.extension().is_some_and(|extension| extension == "md")
        {
            read_skill(&child, agent, skills)?;
        }
    }
    Ok(())
}

fn read_skill(
    path: &Path,
    agent: Agent,
    skills: &mut BTreeMap<String, AgentSkill>,
) -> Result<(), String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let Some(frontmatter) = contents
        .strip_prefix("---\n")
        .or_else(|| contents.strip_prefix("---\r\n"))
    else {
        return Ok(());
    };
    let Some((metadata, _)) = frontmatter.split_once("\n---") else {
        return Ok(());
    };
    let metadata: SkillMetadata = match serde_yaml::from_str(metadata) {
        Ok(metadata) => metadata,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "invalid skill metadata");
            return Ok(());
        }
    };
    if agent == Agent::Claude && metadata.user_invocable == Some(false) {
        return Ok(());
    }
    let Some(name) = metadata.name.filter(|name| !name.trim().is_empty()) else {
        return Ok(());
    };
    let Some(description) = metadata
        .description
        .filter(|description| !description.trim().is_empty())
    else {
        return Ok(());
    };
    let id = if agent == Agent::OpenCode {
        let component = if path.file_name().is_some_and(|file| file == "SKILL.md") {
            path.parent().and_then(Path::file_name)
        } else {
            path.file_stem()
        };
        component
            .and_then(|name| name.to_str())
            .unwrap_or(&name)
            .to_owned()
    } else {
        name.clone()
    };
    let skill = AgentSkill {
        id: id.clone(),
        name,
        description,
        path: path.to_string_lossy().into_owned(),
    };
    if agent == Agent::OpenCode {
        skills.insert(id, skill);
    } else {
        skills.entry(id).or_insert(skill);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_skills_override_global_and_follow_agent_roots()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        let home = temp.path().join("home");
        for root in [
            project.join(".agents/skills/local"),
            home.join(".agents/skills/global"),
            project.join(".claude/skills/claude"),
        ] {
            std::fs::create_dir_all(root)?;
        }
        std::fs::write(
            project.join(".agents/skills/local/SKILL.md"),
            "---\nname: review\ndescription: Project review\n---\n",
        )?;
        std::fs::write(
            home.join(".agents/skills/global/SKILL.md"),
            "---\nname: review\ndescription: Global review\n---\n",
        )?;
        std::fs::write(
            project.join(".claude/skills/claude/SKILL.md"),
            "---\nname: private\ndescription: Hidden\nuser-invocable: false\n---\n",
        )?;
        let codex = discover(
            Agent::Codex,
            &project,
            &home,
            &home.join(".codex"),
            &home.join(".config"),
        )?;
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].description, "Project review");
        assert!(
            discover(
                Agent::Claude,
                &project,
                &home,
                &home.join(".codex"),
                &home.join(".config")
            )?
            .is_empty()
        );
        Ok(())
    }
    #[test]
    fn opencode_uses_the_path_id_for_a_display_name() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        let home = temp.path().join("home");
        let directory = project.join(".opencode/skills/git-release");
        std::fs::create_dir_all(&directory)?;
        std::fs::write(
            directory.join("SKILL.md"),
            "---\nname: Git Release\ndescription: Prepare release notes\n---\n",
        )?;
        let skills = discover(
            Agent::OpenCode,
            &project,
            &home,
            &home.join(".codex"),
            &home.join(".config"),
        )?;
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].id, "git-release");
        assert_eq!(skills[0].name, "Git Release");
        Ok(())
    }
}
