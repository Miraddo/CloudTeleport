//! Caption templates.

use crate::google::DriveFile;

pub const PLACEHOLDERS: &str = "{name} {link} {folder} {size} {type} {created} {modified}";

pub fn render(template: &str, file: &DriveFile, folder: &str) -> String {
    let size = file.size.map(human_size).unwrap_or_default();
    let local = |t: Option<chrono::DateTime<chrono::Utc>>| {
        t.map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
    };
    template
        .replace("{name}", &file.name)
        .replace("{link}", &file.link())
        .replace("{folder}", folder)
        .replace("{size}", &size)
        .replace("{type}", &file.mime_type)
        .replace("{created}", &local(file.created_time))
        .replace("{modified}", &local(file.modified_time))
        .trim()
        .to_string()
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_template() {
        let file = DriveFile {
            id: "abc".into(),
            name: "report.pdf".into(),
            mime_type: "application/pdf".into(),
            size: Some(2048),
            created_time: None,
            modified_time: None,
            web_view_link: Some("https://drive/x".into()),
        };
        assert_eq!(
            render("{name} ({size}) in {folder}: {link}", &file, "Docs"),
            "report.pdf (2.0 KB) in Docs: https://drive/x"
        );
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
