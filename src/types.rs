use std::collections::HashMap;

#[derive(Debug)]
pub struct LocalFileSet {
    pub files: HashMap<String, String>,
    pub directories: Vec<String>,
}

#[derive(Debug)]
pub struct RemoteFileSet {
    pub files: HashMap<String, String>,
    pub directories: Vec<String>,
}
