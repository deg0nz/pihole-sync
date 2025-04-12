use serde_json::{Map, Value};
use std::collections::HashSet;

pub struct ConfigFilter {
    excluded_paths: HashSet<String>,
}

impl ConfigFilter {
    pub fn new(exclude_paths: &[String]) -> Self {
        let excluded_paths: HashSet<String> = exclude_paths.iter().cloned().collect();
        Self { excluded_paths }
    }

    pub fn filter_json(&self, json: Value) -> Value {
        if self.excluded_paths.is_empty() {
            return json;
        }

        self.filter_value(json, String::new())
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

    fn is_path_excluded(&self, path: &str) -> bool {
        // Check if the exact path is excluded
        if self.excluded_paths.contains(path) {
            return true;
        }

        // Check if any parent path is excluded (hierarchical exclusion)
        let mut parts: Vec<&str> = path.split('.').collect();
        while parts.len() > 1 {
            parts.pop();
            let parent_path = parts.join(".");
            if self.excluded_paths.contains(&parent_path) {
                return true;
            }
        }

        false
    }

    fn filter_object(&self, obj: Map<String, Value>, path: String) -> Map<String, Value> {
        let mut filtered_obj = Map::new();

        for (key, value) in obj {
            let current_path = if path.is_empty() {
                key.clone()
            } else {
                format!("{}.{}", path, key)
            };

            if self.is_path_excluded(&current_path) {
                // Skip this property as it's in the exclusion list or its parent is excluded
                continue;
            }

            filtered_obj.insert(key, self.filter_value(value, current_path));
        }

        filtered_obj
    }

    fn filter_array(&self, arr: Vec<Value>, path: String) -> Vec<Value> {
        let mut filtered_arr = Vec::new();

        for (idx, value) in arr.into_iter().enumerate() {
            let current_path = format!("{}[{}]", path, idx);

            if self.is_path_excluded(&current_path) {
                // Skip this array element completely
                continue;
            }

            // Process the array element recursively
            filtered_arr.push(self.filter_value(value, current_path));
        }

        filtered_arr
    }
}
