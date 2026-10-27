use std::{path::PathBuf,process::Command};
fn main(){
    let source=PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../references/upstream/sdocx");
    let head=Command::new("git").arg("-C").arg(&source).args(["rev-parse","HEAD"]).output().expect("pinned source verification required");
    assert!(head.status.success()&&String::from_utf8_lossy(&head.stdout).trim()=="41e9c1099b29a48dc7320501573d7c4858f09303","unexpected renderer source revision");
    let clean=Command::new("git").arg("-C").arg(&source).args(["diff","--quiet","HEAD","--","crates/sdocx","Cargo.lock"]).status().expect("source integrity check required");
    assert!(clean.success(),"renderer source differs from pinned revision");
    println!("cargo:rerun-if-changed={}",source.join(".git/HEAD").display());
    println!("cargo:rerun-if-changed={}",source.join("crates/sdocx/src").display());
}
