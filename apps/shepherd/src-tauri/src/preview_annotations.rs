//! Bounded preview annotation payloads shared by the in-app port and the
//! external preview wrapper. Page content remains untrusted data.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::Url;
use tokio::sync::Mutex;

const MAX_ANNOTATIONS: usize = 50;
const MAX_TARGETS_PER_ANNOTATION: usize = 20;
const MAX_NOTE_BYTES: usize = 4000;
const MAX_FRAMING_BYTES: usize = 8000;
const MAX_SELECTOR_BYTES: usize = 2048;
const MAX_EXCERPT_BYTES: usize = 160;
const MAX_URL_BYTES: usize = 8192;
const MAX_QUEUED_BATCHES: usize = 100;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnnotationBatch {
    pub project_id: String,
    pub task_id: String,
    pub page_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framing: Option<String>,
    pub annotations: Vec<PreviewAnnotation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreviewAnnotation {
    pub id: String,
    pub note: String,
    pub targets: Vec<AnnotationTarget>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnnotationTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computed_styles: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ComponentSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<ScreenshotTarget>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScreenshotTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComponentSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnnotationReceipt {
    pub batch_id: String,
    pub accepted: usize,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AnnotationError {
    #[error("{0}")]
    Invalid(String),
    #[error("The annotation batch does not belong to this preview session.")]
    Forbidden,
    #[error("This annotation was already submitted.")]
    Replayed,
    #[error("The annotation queue is full.")]
    Full,
}

impl AnnotationError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

#[derive(Clone, Default)]
pub(crate) struct AnnotationQueue {
    state: Arc<Mutex<QueueState>>,
}

#[derive(Default)]
struct QueueState {
    ids: BTreeSet<String>,
    batches: Vec<AnnotationBatch>,
}

impl AnnotationQueue {
    pub(crate) async fn accept(
        &self,
        batch: AnnotationBatch,
    ) -> Result<AnnotationReceipt, AnnotationError> {
        let mut state = self.state.lock().await;
        if state.batches.len() >= MAX_QUEUED_BATCHES {
            return Err(AnnotationError::Full);
        }
        if batch
            .annotations
            .iter()
            .any(|annotation| state.ids.contains(&annotation.id))
        {
            return Err(AnnotationError::Replayed);
        }
        let batch_id = uuid::Uuid::new_v4().to_string();
        for annotation in &batch.annotations {
            state.ids.insert(annotation.id.clone());
        }
        let accepted = batch.annotations.len();
        state.batches.push(batch);
        Ok(AnnotationReceipt { batch_id, accepted })
    }

    #[cfg(test)]
    pub(crate) async fn len(&self) -> usize {
        self.state.lock().await.batches.len()
    }

    #[cfg(test)]
    pub(crate) async fn peek(&self) -> Option<usize> {
        self.state
            .lock()
            .await
            .batches
            .first()
            .map(|batch| batch.annotations.len())
    }
}

pub(crate) fn validate_batch(
    mut batch: AnnotationBatch,
    expected_project: &str,
    expected_task: &str,
    expected_origin: &str,
    attachment_root: Option<&Path>,
) -> Result<AnnotationBatch, AnnotationError> {
    if batch.project_id != expected_project || batch.task_id != expected_task {
        return Err(AnnotationError::Forbidden);
    }
    if batch.page_url.len() > MAX_URL_BYTES {
        return Err(AnnotationError::invalid(
            "The annotation page URL is too long.",
        ));
    }
    let page = Url::parse(&batch.page_url)
        .map_err(|_| AnnotationError::invalid("The annotation page URL is invalid."))?;
    if !matches!(page.scheme(), "http" | "https")
        || !page.username().is_empty()
        || page.password().is_some()
        || page.origin().ascii_serialization() != expected_origin
    {
        return Err(AnnotationError::Forbidden);
    }
    batch.page_url = page.to_string();
    if let Some(framing) = &mut batch.framing {
        *framing = framing.trim().to_owned();
        check_text(framing, "The framing message", MAX_FRAMING_BYTES, true)?;
    }
    if batch.annotations.is_empty() || batch.annotations.len() > MAX_ANNOTATIONS {
        return Err(AnnotationError::invalid(
            "An annotation batch must contain between 1 and 50 annotations.",
        ));
    }
    let framing_present = batch
        .framing
        .as_deref()
        .is_some_and(|framing| !framing.is_empty());
    let mut ids = BTreeSet::new();
    for annotation in &mut batch.annotations {
        if annotation.id.len() > 128
            || annotation.id.is_empty()
            || !annotation.id.chars().all(|character| {
                character.is_ascii_alphanumeric() || character == '-' || character == '_'
            })
        {
            return Err(AnnotationError::invalid(
                "Each annotation needs a bounded client-generated ID.",
            ));
        }
        if !ids.insert(annotation.id.clone()) {
            return Err(AnnotationError::invalid(
                "Annotation IDs must be unique within a batch.",
            ));
        }
        annotation.note = annotation.note.trim().to_owned();
        check_text(
            &annotation.note,
            "The annotation note",
            MAX_NOTE_BYTES,
            true,
        )?;
        if annotation.note.is_empty() && !framing_present {
            return Err(AnnotationError::invalid(
                "Write a note for each annotation or frame the batch with the chat message.",
            ));
        }
        if annotation.targets.is_empty() || annotation.targets.len() > MAX_TARGETS_PER_ANNOTATION {
            return Err(AnnotationError::invalid(
                "Each annotation must have between 1 and 20 targets.",
            ));
        }
        for target in &mut annotation.targets {
            validate_target(target, attachment_root)?;
        }
    }
    Ok(batch)
}

fn validate_target(
    target: &mut AnnotationTarget,
    attachment_root: Option<&Path>,
) -> Result<(), AnnotationError> {
    if target.selector.is_some() == target.screenshot.is_some() {
        return Err(AnnotationError::invalid(
            "Each target must have either a selector or screenshot coordinates.",
        ));
    }
    if let Some(selector) = &mut target.selector {
        *selector = selector.trim().to_owned();
        check_text(selector, "The target selector", MAX_SELECTOR_BYTES, false)?;
    }
    if let Some(excerpt) = &mut target.excerpt {
        *excerpt = excerpt.trim().to_owned();
        check_text(excerpt, "The target excerpt", MAX_EXCERPT_BYTES, true)?;
    }
    if let Some(bounds) = target.bounds {
        validate_box(bounds.x, bounds.y, bounds.width, bounds.height, false)?;
    }
    if let Some(styles) = &target.computed_styles {
        if styles.len() > 32 {
            return Err(AnnotationError::invalid(
                "Too many computed styles were attached to one target.",
            ));
        }
        for (name, value) in styles {
            if name.is_empty() || name.len() > 64 || value.len() > 256 {
                return Err(AnnotationError::invalid(
                    "A computed style field is too large.",
                ));
            }
        }
    }
    if let Some(source) = &target.source {
        validate_source(source)?;
    }
    if let Some(screenshot) = &mut target.screenshot {
        validate_box(
            screenshot.x,
            screenshot.y,
            screenshot.width,
            screenshot.height,
            true,
        )?;
        if let Some(path) = &mut screenshot.path {
            if path.len() > 4096 || path.contains('\0') {
                return Err(AnnotationError::invalid("The screenshot path is invalid."));
            }
            let root = attachment_root.ok_or_else(|| {
                AnnotationError::invalid(
                    "Screenshot paths are only accepted from Shepherd's attachment store.",
                )
            })?;
            let root = root.canonicalize().map_err(|_| {
                AnnotationError::invalid("The task attachment directory is unavailable.")
            })?;
            let candidate = PathBuf::from(&*path)
                .canonicalize()
                .map_err(|_| AnnotationError::invalid("The screenshot file is unavailable."))?;
            if !candidate.starts_with(&root) {
                return Err(AnnotationError::invalid(
                    "The screenshot must belong to this task.",
                ));
            }
            *path = candidate.to_string_lossy().into_owned();
        }
    }
    Ok(())
}

fn validate_box(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    screenshot: bool,
) -> Result<(), AnnotationError> {
    if ![x, y, width, height].into_iter().all(f64::is_finite)
        || !(-1_000_000.0..=1_000_000.0).contains(&x)
        || !(-1_000_000.0..=1_000_000.0).contains(&y)
        || !(0.0..=1_000_000.0).contains(&width)
        || !(0.0..=1_000_000.0).contains(&height)
    {
        return Err(AnnotationError::invalid(
            "The target bounds are outside the supported range.",
        ));
    }
    if screenshot
        && (width == 0.0 || height == 0.0 || x < 0.0 || y < 0.0 || x >= width || y >= height)
    {
        return Err(AnnotationError::invalid(
            "The screenshot point must be inside the image.",
        ));
    }
    Ok(())
}

fn validate_source(source: &ComponentSource) -> Result<(), AnnotationError> {
    if let Some(value) = &source.framework {
        check_text(value, "The component framework", 64, false)?;
    }
    if let Some(value) = &source.component {
        check_text(value, "The component name", 256, false)?;
    }
    if let Some(value) = &source.file {
        check_text(value, "The component source path", 512, false)?;
    }
    if source.line == Some(0) || source.column == Some(0) {
        return Err(AnnotationError::invalid(
            "Component source lines start at 1.",
        ));
    }
    Ok(())
}

fn check_text(
    value: &str,
    field: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> Result<(), AnnotationError> {
    if value.len() > max_bytes {
        return Err(AnnotationError::invalid(format!("{field} is too long.")));
    }
    if value.contains('\0') {
        return Err(AnnotationError::invalid(format!(
            "{field} contains a null byte."
        )));
    }
    if !allow_empty && value.trim().is_empty() {
        return Err(AnnotationError::invalid(format!("{field} is required.")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(selector: &str) -> AnnotationTarget {
        AnnotationTarget {
            selector: Some(selector.into()),
            excerpt: Some("Buy now".into()),
            bounds: Some(Bounds {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 32.0,
            }),
            computed_styles: Some(BTreeMap::from([("color".into(), "red".into())])),
            source: Some(ComponentSource {
                framework: Some("react".into()),
                component: Some("Checkout".into()),
                file: Some("src/Checkout.tsx".into()),
                line: Some(12),
                column: Some(3),
            }),
            screenshot: None,
        }
    }

    fn batch(id: &str, note: &str) -> AnnotationBatch {
        AnnotationBatch {
            project_id: "project-1".into(),
            task_id: "task-1".into(),
            page_url: "http://localhost:3000/checkout?step=2".into(),
            framing: None,
            annotations: vec![PreviewAnnotation {
                id: id.into(),
                note: note.into(),
                targets: vec![target("#buy")],
            }],
        }
    }

    #[test]
    fn accepts_a_bounded_element_batch() -> Result<(), Box<dyn std::error::Error>> {
        let normalized = validate_batch(
            batch("annotation-1", "Increase contrast."),
            "project-1",
            "task-1",
            "http://localhost:3000",
            None,
        )?;
        assert_eq!(normalized.annotations[0].note, "Increase contrast.");
        assert_eq!(
            normalized.annotations[0].targets[0].selector.as_deref(),
            Some("#buy")
        );
        Ok(())
    }

    #[test]
    fn allows_an_empty_note_only_with_a_framing_message() {
        let mut request = batch("annotation-1", "");
        assert!(matches!(
            validate_batch(
                request.clone(),
                "project-1",
                "task-1",
                "http://localhost:3000",
                None
            ),
            Err(AnnotationError::Invalid(_))
        ));
        request.framing = Some("Rework the checkout panel.".into());
        assert!(
            validate_batch(
                request,
                "project-1",
                "task-1",
                "http://localhost:3000",
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_wrong_origin_duplicate_ids_and_oversized_notes() {
        assert!(matches!(
            validate_batch(
                batch("annotation-1", "Change this."),
                "project-1",
                "task-1",
                "http://localhost:3001",
                None
            ),
            Err(AnnotationError::Forbidden)
        ));
        let mut duplicate = batch("annotation-1", "Change this.");
        duplicate.annotations.push(PreviewAnnotation {
            id: "annotation-1".into(),
            note: "And this.".into(),
            targets: vec![target("#other")],
        });
        assert!(matches!(
            validate_batch(
                duplicate,
                "project-1",
                "task-1",
                "http://localhost:3000",
                None
            ),
            Err(AnnotationError::Invalid(_))
        ));
        assert!(matches!(
            validate_batch(
                batch("annotation-1", &"x".repeat(MAX_NOTE_BYTES + 1)),
                "project-1",
                "task-1",
                "http://localhost:3000",
                None
            ),
            Err(AnnotationError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_screenshot_paths_outside_the_task_root() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let task = directory.path().join("task-1");
        std::fs::create_dir_all(&task)?;
        let inside = task.join("preview.png");
        std::fs::write(&inside, b"image")?;
        let outside = directory.path().join("outside.png");
        std::fs::write(&outside, b"image")?;

        let mut request = batch("annotation-1", "Move this.");
        request.annotations[0].targets[0] = AnnotationTarget {
            selector: None,
            excerpt: None,
            bounds: None,
            computed_styles: None,
            source: None,
            screenshot: Some(ScreenshotTarget {
                path: Some(inside.to_string_lossy().into_owned()),
                x: 4.0,
                y: 4.0,
                width: 128.0,
                height: 128.0,
            }),
        };
        assert!(
            validate_batch(
                request.clone(),
                "project-1",
                "task-1",
                "http://localhost:3000",
                Some(&task)
            )
            .is_ok()
        );
        request.annotations[0].targets[0]
            .screenshot
            .as_mut()
            .ok_or("missing screenshot")?
            .path = Some(outside.to_string_lossy().into_owned());
        assert!(matches!(
            validate_batch(
                request,
                "project-1",
                "task-1",
                "http://localhost:3000",
                Some(&task)
            ),
            Err(AnnotationError::Invalid(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_replayed_annotation_ids() -> Result<(), Box<dyn std::error::Error>> {
        let queue = AnnotationQueue::default();
        let first = validate_batch(
            batch("annotation-1", "Change this."),
            "project-1",
            "task-1",
            "http://localhost:3000",
            None,
        )?;
        queue.accept(first.clone()).await?;
        assert_eq!(queue.len().await, 1);
        assert!(queue.peek().await.is_some());
        assert!(matches!(
            queue.accept(first).await,
            Err(AnnotationError::Replayed)
        ));
        Ok(())
    }
}
