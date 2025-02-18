use crate::{
    statics::{
        regexes::{IS_ONLY_SYMBOLS_RE, PLUGINS_REGEXPS},
        HASHER, NEW_LINE, SYMBOLS,
    },
    types::{EachLine, EngineType, GameType, OptionExt, ProcessingMode, ResultExt},
};
use indexmap::IndexSet;
use marshal_rs::{load, StringMode};
use sonic_rs::{from_str, prelude::*, Object, Value};
use std::{
    collections::{HashSet, VecDeque},
    ffi::OsString,
    fs::{read, DirEntry, File},
    io::{self, BufReader, Read},
    path::Path,
    str::from_utf8_unchecked,
};
use xxhash_rust::xxh3::Xxh3DefaultBuilder;

#[inline]
pub fn romanize_string(string: String) -> String {
    let mut result: String = String::with_capacity(string.capacity());

    for char in string.chars() {
        let replacement: &str = match char {
            '。' => ".",
            '、' | '，' => ",",
            '・' => "·",
            '゠' => "–",
            '＝' | 'ー' => "—",
            '「' | '」' | '〈' | '〉' => "'",
            '『' | '』' | '《' | '》' => "\"",
            '（' | '〔' | '｟' | '〘' => "(",
            '）' | '〕' | '｠' | '〙' => ")",
            '｛' => "{",
            '｝' => "}",
            '［' | '【' | '〖' | '〚' => "[",
            '］' | '】' | '〗' | '〛' => "]",
            '〜' => "~",
            '？' => "?",
            '！' => "!",
            '：' => ":",
            '※' => "·",
            '…' | '‥' => "...",
            '　' => " ",
            'Ⅰ' => "I",
            'ⅰ' => "i",
            'Ⅱ' => "II",
            'ⅱ' => "ii",
            'Ⅲ' => "III",
            'ⅲ' => "iii",
            'Ⅳ' => "IV",
            'ⅳ' => "iv",
            'Ⅴ' => "V",
            'ⅴ' => "v",
            'Ⅵ' => "VI",
            'ⅵ' => "vi",
            'Ⅶ' => "VII",
            'ⅶ' => "vii",
            'Ⅷ' => "VIII",
            'ⅷ' => "viii",
            'Ⅸ' => "IX",
            'ⅸ' => "ix",
            'Ⅹ' => "X",
            'ⅹ' => "x",
            'Ⅺ' => "XI",
            'ⅺ' => "xi",
            'Ⅻ' => "XII",
            'ⅻ' => "xii",
            'Ⅼ' => "L",
            'ⅼ' => "l",
            'Ⅽ' => "C",
            'ⅽ' => "c",
            'Ⅾ' => "D",
            'ⅾ' => "d",
            'Ⅿ' => "M",
            'ⅿ' => "m",
            _ => {
                result.push(char);
                continue;
            }
        };

        result.push_str(replacement);
    }

    result
}

#[inline(always)]
pub fn get_object_data(object: &Object) -> Vec<u8> {
    let mut vec: Vec<u8> = Vec::new();

    if let Some(object_type) = object.get(&"__type") {
        if object_type.as_str().is_some_and(|_type: &str| _type == "bytes") {
            vec = unsafe { sonic_rs::from_value::<Vec<u8>>(&object["data"]).unwrap_unchecked() };
        }
    }

    vec
}

#[inline]
pub fn extract_strings(
    ruby_code: &str,
    write: bool,
) -> (IndexSet<String, Xxh3DefaultBuilder>, Vec<std::ops::Range<usize>>) {
    fn is_escaped(index: usize, string: &str) -> bool {
        let mut backslash_count: u8 = 0;

        for char in string[..index].chars().rev() {
            if char == '\\' {
                backslash_count += 1;
            } else {
                break;
            }
        }

        backslash_count % 2 == 1
    }

    let mut strings: IndexSet<String, Xxh3DefaultBuilder> = IndexSet::with_hasher(HASHER);
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let mut inside_string: bool = false;
    let mut inside_multiline_comment: bool = false;
    let mut string_start_index: usize = 0;
    let mut current_quote_type: char = '\0';
    let mut global_index: usize = 0;

    for line in ruby_code.each_line() {
        let trimmed: &str = line.trim();

        if !inside_string {
            if trimmed.starts_with('#') {
                global_index += line.len();
                continue;
            }

            if trimmed.starts_with("=begin") {
                inside_multiline_comment = true;
            } else if trimmed.starts_with("=end") {
                inside_multiline_comment = false;
            }
        }

        if inside_multiline_comment {
            global_index += line.len();
            continue;
        }

        let char_indices: std::str::CharIndices = line.char_indices();

        for (i, char) in char_indices {
            if !inside_string && char == '#' {
                break;
            }

            if !inside_string && (char == '"' || char == '\'') {
                inside_string = true;
                string_start_index = global_index + i;
                current_quote_type = char;
            } else if inside_string && char == current_quote_type && !is_escaped(i, &line) {
                let range: std::ops::Range<usize> = string_start_index + 1..global_index + i;

                let extracted_string: String = ruby_code[range.clone()]
                    .replace("\r\n", NEW_LINE)
                    .replace('\n', NEW_LINE);

                if !extracted_string.is_empty() && !strings.contains(&extracted_string) {
                    strings.insert(extracted_string);

                    if write {
                        ranges.push(range);
                    }
                }

                inside_string = false;
                current_quote_type = '\0';
            }
        }

        global_index += line.len();
    }

    (strings, ranges)
}

#[inline(always)]
pub const fn determine_extension(engine_type: EngineType) -> &'static str {
    match engine_type {
        EngineType::New => ".json",
        EngineType::VXAce => ".rvdata2",
        EngineType::VX => ".rvdata",
        EngineType::XP => ".rxdata",
    }
}

#[inline]
pub fn filter_maps(entry: Result<DirEntry, std::io::Error>, engine_type: EngineType) -> Option<(String, Value)> {
    if let Ok(entry) = entry {
        if !entry.file_type().unwrap_log().is_file() {
            return None;
        };

        let filename: OsString = entry.file_name();
        let filename_str: &str = unsafe { from_utf8_unchecked(filename.as_encoded_bytes()) };

        if filename_str.starts_with("Map")
            && unsafe { (*filename_str.as_bytes().get_unchecked(3) as char).is_ascii_digit() }
            && filename_str.ends_with(determine_extension(engine_type))
        {
            let json: Value = if engine_type == EngineType::New {
                from_str(&read_to_string_without_bom(entry.path()).unwrap_log()).unwrap_log()
            } else {
                load(&read(entry.path()).unwrap_log(), Some(StringMode::UTF8), Some("")).unwrap_log()
            };

            Some((filename_str.to_owned(), json))
        } else {
            None
        }
    } else {
        None
    }
}

#[inline]
pub fn filter_other(
    entry: Result<DirEntry, std::io::Error>,
    engine_type: EngineType,
    game_type: Option<GameType>,
) -> Option<(String, Value)> {
    if let Ok(entry) = entry {
        if !entry.file_type().unwrap_log().is_file() {
            return None;
        };

        let filename: OsString = entry.file_name();
        let filename_str: &str = unsafe { from_utf8_unchecked(filename.as_encoded_bytes()) };

        let (name, _) = unsafe { filename_str.split_once('.').unwrap_unchecked() };

        if !name.starts_with("Map")
            && !matches!(name, "Areas" | "Tilesets" | "Animations" | "System" | "Scripts")
            && filename_str.ends_with(determine_extension(engine_type))
        {
            if game_type.is_some_and(|game_type: GameType| game_type == GameType::Termina) && name == "States" {
                return None;
            }

            let json: Value = if engine_type == EngineType::New {
                from_str(&read_to_string_without_bom(entry.path()).unwrap_log()).unwrap_log()
            } else {
                load(&read(entry.path()).unwrap_log(), Some(StringMode::UTF8), Some("")).unwrap_log()
            };

            Some((filename_str.to_owned(), json))
        } else {
            None
        }
    } else {
        None
    }
}

#[inline(always)]
pub const fn get_system_labels(
    engine_type: EngineType,
) -> (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
) {
    match engine_type {
        EngineType::New => (
            "armorTypes",
            "elements",
            "skillTypes",
            "terms",
            "weaponTypes",
            "gameTitle",
        ),
        EngineType::XP => (
            "__symbol__armor_types",
            "__symbol__elements",
            "__symbol__skill_types",
            "__symbol__words",
            "__symbol__weapon_types",
            "__symbol__game_title",
        ),
        _ => (
            "__symbol__armor_types",
            "__symbol__elements",
            "__symbol__skill_types",
            "__symbol__terms",
            "__symbol__weapon_types",
            "__symbol__game_title",
        ),
    }
}

#[inline(always)]
pub const fn get_maps_labels(
    engine_type: EngineType,
) -> (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
) {
    match engine_type {
        EngineType::New => ("displayName", "events", "pages", "list", "code", "parameters"),
        _ => (
            "__symbol__display_name",
            "__symbol__events",
            "__symbol__pages",
            "__symbol__list",
            "__symbol__code",
            "__symbol__parameters",
        ),
    }
}

#[allow(clippy::type_complexity)]
#[inline(always)]
pub const fn get_other_labels(
    engine_type: EngineType,
) -> (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
) {
    match engine_type {
        EngineType::New => (
            "name",
            "nickname",
            "description",
            "message1",
            "message2",
            "message3",
            "message4",
            "note",
            "pages",
            "list",
            "code",
            "parameters",
        ),
        _ => (
            "__symbol__name",
            "__symbol__nickname",
            "__symbol__description",
            "__symbol__message1",
            "__symbol__message2",
            "__symbol__message3",
            "__symbol__message4",
            "__symbol__note",
            "__symbol__pages",
            "__symbol__list",
            "__symbol__code",
            "__symbol__parameters",
        ),
    }
}

#[inline]
pub fn read_to_string_without_bom<P: AsRef<Path>>(file_path: P) -> io::Result<String> {
    const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

    let file: File = File::open(file_path.as_ref())?;
    let mut reader: BufReader<File> = BufReader::new(file);

    let mut buffer: [u8; 3] = [0u8; 3];
    let mut content: String = String::new();

    let read_bytes: usize = reader.read(&mut buffer)?;

    if read_bytes == 3 && buffer == BOM {
        reader.read_to_string(&mut content)?;
    } else {
        reader.seek_relative(-3)?;
        reader.read_to_string(&mut content)?;
    }

    Ok(content)
}

#[allow(clippy::too_many_arguments)]
pub fn traverse_json(
    key: Option<&str>,
    value: &mut Value,
    lines: &mut Option<&mut Vec<String>>,
    map: &mut Option<&mut VecDeque<(String, String)>>,
    set: &Option<&HashSet<String, Xxh3DefaultBuilder>>,
    write: bool,
    romanize: bool,
    processing_mode: ProcessingMode,
) {
    let invalid_key = |key: &Option<&str>| -> bool {
        if let Some(str) = key {
            if str.starts_with("LATIN") {
                false
            } else {
                PLUGINS_REGEXPS.iter().any(|re| re.is_match(str))
            }
        } else {
            false
        }
    };

    match value.get_type() {
        sonic_rs::JsonType::String => {
            if invalid_key(&key) {
                return;
            }

            let str: &str = unsafe { value.as_str().unwrap_unchecked() }.trim();

            if !(str.is_empty()
                || IS_ONLY_SYMBOLS_RE.is_match(str)
                || ["true", "false", "none", "time", "off"].contains(&str)
                || str.starts_with("this.")
                    && str.chars().nth(5).is_some_and(|c: char| c.is_alphabetic())
                    && str.ends_with(")")
                || str.starts_with("rgba"))
                || key.is_some_and(|x| x.starts_with("LATIN"))
            {
                let mut string: String = str.replace('\n', NEW_LINE);

                if romanize {
                    string = romanize_string(string);
                }

                if write {
                    if !unsafe { set.as_ref().unwrap_unchecked() }.contains(&string) {
                        return;
                    }

                    if let Some((_, translated)) = unsafe { map.as_mut().unwrap_unchecked().pop_front() } {
                        *value = (&translated).into();
                    }
                } else {
                    let lines = unsafe { lines.as_mut().unwrap_unchecked() };

                    lines.push(string);
                    let last: &String = unsafe { lines.last().unwrap_unchecked() };
                    let pos: usize = lines.len() - 1;

                    if processing_mode == ProcessingMode::Append {
                        let translation_map = unsafe { map.as_mut().unwrap_unchecked() };

                        if translation_map.get(pos).is_some_and(|x| *last != x.0) {
                            translation_map.insert(pos, (last.to_owned(), String::new()));
                        }
                    }
                }
            }
        }
        sonic_rs::JsonType::Object => {
            for (key, value) in value.as_object_mut().unwrap_log() {
                traverse_json(Some(key), value, lines, map, set, write, romanize, processing_mode);
            }
        }
        sonic_rs::JsonType::Array => {
            for value in value.as_array_mut().unwrap_log() {
                traverse_json(None, value, lines, map, set, write, romanize, processing_mode);
            }
        }
        _ => {}
    }
}

#[inline(always)]
pub fn string_is_only_symbols(string: &str) -> bool {
    !string.chars().any(|c| !SYMBOLS.contains(&c))
}

#[inline]
pub fn ends_with_if_index(string: &str) -> Option<usize> {
    if string.ends_with(')') {
        let mut step: u8 = 0;
        let chars = string.char_indices().rev().skip(1);

        for (i, char) in chars {
            match step {
                0 => {
                    if char == '(' {
                        step = 1;
                    }
                }
                1 => {
                    if char == 'f' {
                        step = 2;
                    } else {
                        return None;
                    }
                }
                2 => {
                    if char == 'i' {
                        step = 3;
                    } else {
                        return None;
                    }
                }
                3 => {
                    if char == ' ' {
                        return Some(i);
                    }
                }
                _ => unreachable!(),
            }
        }
    }

    None
}

#[inline]
pub fn find_lisa_prefix_index(string: &str) -> Option<usize> {
    if string.starts_with(r"\et") {
        let mut index: usize = 5;

        loop {
            let char: &str = unsafe { string.get(index..index + 1).unwrap_unchecked() };

            if char != "]" {
                index += 1;
            } else {
                return Some(index + 1);
            }

            if index == 10 {
                return None;
            }
        }
    } else if string.starts_with(r"\nbt") {
        Some(4)
    } else {
        None
    }
}

/// 401 - Dialogue line.
///
/// 101 - Start of the dialogue line. (**XP ENGINE ONLY!**)
///
/// 102 - Dialogue choices array.
///
/// 402 - One of the dialogue choices from the array. (**WRITE ONLY!**)
///
/// 405 - Credits lines. (**probably NEWER ENGINES ONLY!**)
///
/// 356 - System line, special text. (that one needs clarification)
///
/// 655 - Line displayed in shop - from an external script. (**OLDER ENGINES ONLY!**)
///
/// 324, 320 - Some used in-game line. (**probably NEWER ENGINES ONLY!**)
#[inline(always)]
pub const fn is_allowed_code(code: u16) -> bool {
    matches!(code, 101 | 102 | 320 | 324 | 356 | 401 | 402 | 405 | 655)
}

#[inline(always)]
pub fn parse_map_number(string: &str) -> u16 {
    unsafe {
        String::from_utf8_unchecked(
            string
                .as_bytes()
                .iter()
                .filter(|c| c.is_ascii_digit())
                .take(3)
                .copied()
                .collect(),
        )
        .parse::<u16>()
        .unwrap_unchecked()
    }
}
