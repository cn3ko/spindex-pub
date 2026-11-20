//! Rekordbox adapter via its official XML. SP1 writes; reading is SP2.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};

use super::capabilities::{Capabilities, REKORDBOX_XML};
use super::endpoint::{DjEndpoint, ReadOutcome, WriteOutcome, CANCELLED};
use super::model::DjLibrary;

pub struct RekordboxXmlEndpoint {
    pub out_path: PathBuf,
    pub product_version: String,
}

impl DjEndpoint for RekordboxXmlEndpoint {
    fn capabilities(&self) -> &'static Capabilities {
        &REKORDBOX_XML
    }

    fn read(&self, _progress: &dyn Fn(usize, usize), _cancel: &AtomicBool) -> Result<ReadOutcome> {
        bail!("reading Rekordbox XML is sub-project 2")
    }

    /// Builds the XML in memory, writes it to a temp file beside the target
    /// and renames it into place: a failure or cancel leaves the previous
    /// export untouched.
    fn write(&self, lib: &DjLibrary, cancel: &AtomicBool) -> Result<WriteOutcome> {
        if cancel.load(Ordering::SeqCst) {
            bail!(CANCELLED);
        }
        let xml = crate::core::rekordbox_xml::write_xml(lib, &self.product_version);
        let dir = self.out_path.parent().context("export path has no folder")?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let tmp = self.out_path.with_extension("xml.spindex-tmp");
        let res = std::fs::write(&tmp, xml.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))
            .and_then(|()| {
                std::fs::rename(&tmp, &self.out_path)
                    .with_context(|| format!("replacing {}", self.out_path.display()))
            });
        if res.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        res?;
        Ok(WriteOutcome { written: self.out_path.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_creates_the_file_atomically_and_replaces_an_old_one() {
        let dir = std::env::temp_dir().join(format!("spindex-rbx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out = dir.join("sub").join("rekordbox-USB.xml");
        let ep = RekordboxXmlEndpoint { out_path: out.clone(), product_version: "0.3.2".into() };
        let cancel = AtomicBool::new(false);
        ep.write(&DjLibrary::default(), &cancel).unwrap();
        let first = std::fs::read_to_string(&out).unwrap();
        assert!(first.contains("<COLLECTION Entries=\"0\">"));
        ep.write(&DjLibrary::default(), &cancel).unwrap(); // replaces in place
        let names: Vec<String> = std::fs::read_dir(out.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["rekordbox-USB.xml"], "no temp file left behind");
    }

    #[test]
    fn a_cancelled_write_leaves_the_previous_file() {
        let dir = std::env::temp_dir().join(format!("spindex-rbx-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("x.xml");
        std::fs::write(&out, "old").unwrap();
        let ep = RekordboxXmlEndpoint { out_path: out.clone(), product_version: "0.3.2".into() };
        assert!(ep.write(&DjLibrary::default(), &AtomicBool::new(true)).is_err());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "old");
    }
}
