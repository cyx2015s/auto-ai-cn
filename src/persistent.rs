use serde_json::ser::PrettyFormatter;

pub struct Persistent<T, S>
where
    T: serde::Serialize,
    for<'b> &'b mut S: serde::Serializer,
{
    value: T,
    serializer: S,
}

impl<T, S> Persistent<T, S>
where
    T: serde::Serialize,
    for<'b> &'b mut S: serde::Serializer,
{
    pub fn new(value: T, serializer: S) -> Self {
        Self { value, serializer }
    }
}

pub fn persistent<T, S>(value: T, serializer: S) -> Persistent<T, S>
where
    T: serde::Serialize,
    for<'b> &'b mut S: serde::Serializer,
{
    Persistent::new(value, serializer)
}

pub fn persistent_via_file_with_default<T, P>(
    path: P,
) -> Persistent<T, serde_json::Serializer<std::fs::File, PrettyFormatter<'static>>>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Default,
    P: AsRef<std::path::Path>,
{
    persistent_via_file(T::default(), path)
}

pub fn persistent_via_file<T, P>(
    value: T,
    path: P,
) -> Persistent<T, serde_json::Serializer<std::fs::File, PrettyFormatter<'static>>>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
    P: AsRef<std::path::Path>,
{
    let file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .create(true)
        .open(&path)
        .expect("...");
    Persistent::new(value, serde_json::Serializer::pretty(file))
}

impl<T, S> Drop for Persistent<T, S>
where
    T: serde::Serialize,
    for<'b> &'b mut S: serde::Serializer,
{
    fn drop(&mut self) {
        let r = self.value.serialize(&mut self.serializer);
        if r.is_err() {
            log::error!("Failed to serialize value: {:?}", r.err());
        }
    }
}

impl<T, S> std::ops::Deref for Persistent<T, S>
where
    T: serde::Serialize,
    for<'b> &'b mut S: serde::Serializer,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T, S> std::ops::DerefMut for Persistent<T, S>
where
    T: serde::Serialize,
    for<'b> &'b mut S: serde::Serializer,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;
    use serde_json::Serializer;

    #[test]
    fn test_persistent() {
        let mut output = Vec::new();
        let serializer = Serializer::pretty(&mut output);
        {
            let mut persistent_vec = Persistent::new(vec![1, 2, 3], serializer);
            persistent_vec.push(4);
            persistent_vec.push(5);
        }
        assert_eq!(output, b"[1,2,3,4,5]");
    }

    #[test]
    fn test_file_persistence() {
        #[derive(Debug, serde::Serialize, serde::Deserialize)]
        struct MyData {
            numbers: Vec<i32>,
            string: String,
        }
        let path = "test_persistent.json";

        {
            let mut persistent_data = persistent_via_file(
                MyData {
                    numbers: vec![1, 2, 3],
                    string: "hello".into(),
                },
                path,
            );
            persistent_data.numbers.push(4);
            persistent_data.numbers.push(5);
        }

        let file = std::fs::File::open(path).unwrap();
        let deserialized: MyData = serde_json::from_reader(file).unwrap();
        dbg!(deserialized);
        let mut file = std::fs::File::open(path).unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, r#"{"numbers":[1,2,3,4,5],"string":"hello"}"#);
        std::fs::remove_file(path).unwrap();
    }
}
