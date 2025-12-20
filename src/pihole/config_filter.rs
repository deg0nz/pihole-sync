use serde_json::{Map, Value};
use std::collections::HashSet;

pub enum FilterMode {
    OptIn,  // Only include specified paths
    OptOut, // Include everything except specified paths
}

pub struct ConfigFilter {
    paths: HashSet<String>,
    mode: FilterMode,
}

impl ConfigFilter {
    pub fn new(paths: &[String], mode: FilterMode) -> Self {
        let paths: HashSet<String> = paths.iter().cloned().collect();
        Self { paths, mode }
    }

    pub fn filter_json(&self, json: Value) -> Value {
        if self.paths.is_empty() {
            match self.mode {
                FilterMode::OptIn => Value::Object(Map::new()), // Empty result if nothing opted in
                FilterMode::OptOut => json, // Everything included if nothing opted out
            }
        } else {
            self.filter_value(json, String::new())
        }
    }

    fn filter_value(&self, value: Value, current_path: String) -> Value {
        match value {
            Value::Object(obj) => {
                let filtered_obj = self.filter_object(obj, current_path);
                Value::Object(filtered_obj)
            }
            Value::Array(arr) => {
                let filtered_arr = self.filter_array(arr, current_path);
                Value::Array(filtered_arr)
            }
            _ => value,
        }
    }

    fn should_include_path(&self, path: &str) -> bool {
        match self.mode {
            FilterMode::OptIn => {
                // Check if the exact path is included
                if self.paths.contains(path) {
                    return true;
                }

                // Check if any parent path is included
                let base_path = path.split('[').next().unwrap(); // Get path without array index
                let mut parts: Vec<&str> = base_path.split('.').collect();

                // Check the exact parent path (for array elements)
                if self.paths.contains(&parts.join(".")) {
                    return true;
                }

                while parts.len() > 1 {
                    parts.pop();
                    let parent_path = parts.join(".");
                    if self.paths.contains(&parent_path) {
                        return true;
                    }
                }

                // Check if this is a parent of any included path
                self.paths.iter().any(|included_path| {
                    included_path.starts_with(base_path)
                        && (included_path.len() == base_path.len()
                            || included_path.chars().nth(base_path.len()) == Some('.'))
                })
            }
            FilterMode::OptOut => {
                // Check if the exact path is excluded
                if self.paths.contains(path) {
                    return false;
                }

                // Check if any parent path is excluded (hierarchical exclusion)
                let mut parts: Vec<&str> = path.split('.').collect();
                while parts.len() > 1 {
                    parts.pop();
                    let parent_path = parts.join(".");
                    if self.paths.contains(&parent_path) {
                        return false;
                    }
                }
                true
            }
        }
    }

    fn filter_object(&self, obj: Map<String, Value>, path: String) -> Map<String, Value> {
        let mut filtered_obj = Map::new();

        for (key, value) in obj {
            let current_path = if path.is_empty() {
                key.clone()
            } else {
                format!("{}.{}", path, key)
            };

            let should_include = self.should_include_path(&current_path);

            if should_include {
                filtered_obj.insert(key, self.filter_value(value, current_path));
            }
        }

        filtered_obj
    }

    fn filter_array(&self, arr: Vec<Value>, path: String) -> Vec<Value> {
        let mut filtered_arr = Vec::new();

        for (idx, value) in arr.into_iter().enumerate() {
            let current_path = format!("{}[{}]", path, idx);

            let should_include = self.should_include_path(&current_path);

            if should_include {
                filtered_arr.push(self.filter_value(value, current_path));
            }
        }

        filtered_arr
    }
}
