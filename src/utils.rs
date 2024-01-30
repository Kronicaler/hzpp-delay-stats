/// Get a substring between 2 strings
pub fn str_between_str<'a>(full_str: &'a str, str1: &str, str2: &str) -> Option<&'a str> {
    let x = full_str.find(str1)? + str1.len();

    let y = full_str.find(str2)?;

    let result = &full_str[x..y];

    Some(result)
}
