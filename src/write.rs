#![allow(clippy::too_many_arguments)]
#[cfg(feature = "log")]
use crate::{eprintln, println};
use crate::{
    functions::{
        determine_extension, ends_with_if_index, extract_strings, filter_maps, filter_other, find_lisa_prefix_index,
        get_maps_labels, get_object_data, get_other_labels, get_system_labels, is_allowed_code, parse_map_number,
        read_to_string_without_bom, romanize_string, string_is_only_symbols, traverse_json,
    },
    statics::{
        localization::{AT_POSITION_MSG, COULD_NOT_SPLIT_LINE_MSG, IN_FILE_MSG, WROTE_FILE_MSG},
        ENCODINGS, HASHER, LINES_SEPARATOR, NEW_LINE,
    },
    types::{
        Code, EngineType, GameType, MapsProcessingMode, OptionExt, ProcessingMode, ResultExt, TrimReplace, Variable,
    },
};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use marshal_rs::{dump, load, StringMode};
use rayon::prelude::*;
use sonic_rs::{from_str, from_value, json, prelude::*, to_string, to_vec, Array, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsStr,
    fs::{read, read_dir, read_to_string, write},
    io::{Read, Write},
    mem::{take, transmute},
    path::Path,
    str::Chars,
    sync::{Arc, Mutex},
};
use xxhash_rust::xxh3::Xxh3DefaultBuilder;

type StringHashMap = HashMap<String, String, Xxh3DefaultBuilder>;

#[inline]
fn parse_translation<'a>(translation: &'a str, file: &'a str) -> Box<dyn Iterator<Item = (String, String)> + 'a> {
    Box::new(translation.split('\n').enumerate().filter_map(move |(i, line)| {
        if line.starts_with("<!--") {
            None
        } else if let Some((original, translated)) = line.split_once(LINES_SEPARATOR) {
            #[cfg(not(debug_assertions))]
            if translated.is_empty() {
                return None;
            }

            Some((
                original.replace(NEW_LINE, "\n").trim_replace(),
                translated.replace(NEW_LINE, "\n").trim_replace(),
            ))
        } else {
            eprintln!(
                "{COULD_NOT_SPLIT_LINE_MSG} ({line})\n{AT_POSITION_MSG} {i}\n{IN_FILE_MSG} {file}",
                i = i + 1
            );
            None
        }
    }))
}

#[inline]
fn process_parameter(
    code: Code,
    mut parameter: String,
    map: Option<&StringHashMap>,
    deque: Option<Arc<Mutex<VecDeque<String>>>>,
    game_type: Option<GameType>,
    engine_type: EngineType,
    romanize: bool,
    value: &mut Value,
) {
    if romanize {
        parameter = romanize_string(parameter);
    }

    let translated: Option<String> = get_translated_parameter(code, &parameter, map, deque, game_type, engine_type);

    if let Some(mut translated) = translated {
        if code == Code::Shop {
            let left: &str = unsafe { parameter.split_once('=').unwrap_unchecked().0 };
            translated = format!("{left}=\"{translated}\"");
        }

        *value = if engine_type == EngineType::New {
            Value::from(&translated)
        } else {
            json!({"__type": "bytes", "data": Array::from(translated.as_bytes())})
        };
    }
}

#[allow(clippy::single_match, clippy::match_single_binding, unused_mut)]
#[inline]
fn get_translated_parameter(
    code: Code,
    mut parameter: &str,
    hashmap: Option<&StringHashMap>,
    deque: Option<Arc<Mutex<VecDeque<String>>>>,
    game_type: Option<GameType>,
    engine_type: EngineType,
) -> Option<String> {
    // bool indicates insert whether at start or at end
    // true inserts at end
    // false inserts at start
    let mut remaining_strings: Vec<(&str, bool)> = Vec::with_capacity(4);

    #[allow(unreachable_patterns)]
    if let Some(game_type) = game_type {
        match game_type {
            GameType::Termina => match code {
                Code::System => {
                    if !parameter.starts_with("Gab")
                        && (!parameter.starts_with("choice_text") || parameter.ends_with("????"))
                    {
                        return None;
                    }
                }
                _ => {}
            },
            GameType::LisaRPG => match code {
                Code::Dialogue | Code::DialogueStart => {
                    if let Some(i) = find_lisa_prefix_index(parameter) {
                        if string_is_only_symbols(&parameter[i..]) {
                            return None;
                        }

                        remaining_strings.push((&parameter[..i], false));

                        if !parameter.starts_with(r"\et") {
                            parameter = &parameter[i..];
                        }
                    }
                }
                _ => {}
            },
            _ => {} // custom processing for other games
        }
    }

    if engine_type != EngineType::New {
        if let Some(i) = ends_with_if_index(parameter) {
            remaining_strings.push((&parameter[..i], true));
            parameter = &parameter[..i];
        }

        match code {
            Code::Shop => {
                if !parameter.contains("shop_talk") {
                    return None;
                }

                let actual_string: &str = unsafe { parameter.split_once('=').unwrap_unchecked().1 }.trim();
                let without_quotes: &str = &actual_string[1..actual_string.len() - 1];

                if string_is_only_symbols(without_quotes) {
                    return None;
                }

                parameter = without_quotes;
            }
            _ => {}
        }
    }

    let translated: Option<String> = if let Some(hashmap) = hashmap {
        hashmap.get(parameter).map(|translated: &String| {
            let mut result: String = translated.to_owned();
            result
        })
    } else {
        let deque: &mut VecDeque<String> = &mut deque.as_ref().unwrap_log().lock().unwrap_log();

        if code == Code::ChoiceArray {
            deque.front().map(String::to_owned)
        } else {
            deque.pop_front()
        }
    };

    if let Some(mut translated) = translated {
        for (string, position) in remaining_strings.into_iter() {
            match position {
                false => translated = string.to_owned() + &translated,
                true => translated += string,
            }
        }

        Some(translated)
    } else {
        translated
    }
}

#[allow(clippy::single_match, clippy::match_single_binding, unused_mut)]
#[inline]
fn get_translated_variable(
    mut variable_text: String,
    note_text: Option<&str>, // note_text is some only when getting description
    variable_type: Variable,
    filename: &str,
    hashmap: &StringHashMap,
    game_type: Option<GameType>,
    engine_type: EngineType,
) -> Option<String> {
    // bool indicates insert whether at start or at end
    // true inserts at end
    // false inserts at start
    let mut remaining_strings: Vec<(String, bool)> = Vec::with_capacity(4);

    if engine_type != EngineType::New {
        variable_text = variable_text.replace("\r\n", "\n");
    }

    #[allow(clippy::collapsible_match)]
    if let Some(game_type) = game_type {
        match game_type {
            GameType::Termina => match variable_type {
                Variable::Description => match note_text {
                    Some(mut note) => {
                        let mut note_string: String = String::from(note);

                        let mut note_chars: Chars = note.chars();
                        let mut is_continuation_of_description: bool = false;

                        if !note.starts_with("flesh puppetry") {
                            if let Some(first_char) = note_chars.next() {
                                if let Some(second_char) = note_chars.next() {
                                    if ((first_char == '\n' && second_char != '\n')
                                        || (first_char.is_ascii_alphabetic()
                                            || first_char == '"'
                                            || note.starts_with("4 sticks")))
                                        && !matches!(first_char, '.' | '!' | '/' | '?')
                                    {
                                        is_continuation_of_description = true;
                                    }
                                }
                            }
                        }

                        if is_continuation_of_description {
                            if let Some((mut left, _)) = note.trim_start().split_once('\n') {
                                left = left.trim();

                                if left.ends_with(['.', '%', '!', '"']) {
                                    note_string = String::from("\n") + left;
                                }
                            } else if note.ends_with(['.', '%', '!', '"']) {
                                note_string = note.to_owned();
                            }

                            if !note_string.is_empty() {
                                variable_text = variable_text + &note_string;
                            }
                        }
                    }
                    None => {}
                },
                Variable::Message1 | Variable::Message2 | Variable::Message3 | Variable::Message4 => {
                    return None;
                }
                Variable::Note => {
                    if filename.starts_with("It") {
                        for string in [
                            "<Menu Category: Items>",
                            "<Menu Category: Food>",
                            "<Menu Category: Healing>",
                            "<Menu Category: Body bag>",
                        ] {
                            if variable_text.contains(string) {
                                variable_text = variable_text.replace(string, &hashmap[string]);
                            }
                        }
                    }

                    if !filename.starts_with("Cl") {
                        let mut variable_text_chars: Chars = variable_text.chars();
                        let mut is_continuation_of_description: bool = false;

                        if let Some(first_char) = variable_text_chars.next() {
                            if let Some(second_char) = variable_text_chars.next() {
                                if ((first_char == '\n' && second_char != '\n')
                                    || (first_char.is_ascii_alphabetic()
                                        || first_char == '"'
                                        || variable_text.starts_with("4 sticks")))
                                    && !matches!(first_char, '.' | '!' | '/' | '?')
                                {
                                    is_continuation_of_description = true;
                                }
                            }
                        }

                        return if is_continuation_of_description {
                            if let Some((_, right)) = variable_text.trim_start().split_once('\n') {
                                Some(right.to_owned())
                            } else {
                                Some(String::new())
                            }
                        } else {
                            Some(variable_text)
                        };
                    }
                }
                _ => {}
            },
            _ => {} // custom processing for other games
        }
    }

    let translated: Option<String> = hashmap.get(&variable_text).map(|translated: &String| {
        let mut result: String = translated.to_owned();

        for (string, position) in remaining_strings.into_iter() {
            match position {
                true => result += &string,
                false => result = string + &result,
            }
        }

        if matches!(
            variable_type,
            Variable::Message1 | Variable::Message2 | Variable::Message3 | Variable::Message4
        ) && !(variable_type == Variable::Message2 && filename.starts_with("Sk"))
        {
            result = String::from(" ") + &result;
        }

        #[allow(clippy::collapsible_if, clippy::collapsible_match)]
        if let Some(game_type) = game_type {
            match game_type {
                GameType::Termina => match variable_type {
                    Variable::Note => {
                        if let Some(first_char) = result.chars().next() {
                            if first_char != '\n' {
                                result = String::from("\n") + &result
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        result
    });

    translated
}

#[inline]
fn write_list(
    list: &mut Array,
    romanize: bool,
    game_type: Option<GameType>,
    engine_type: EngineType,
    map: &StringHashMap,
    deque: Option<Arc<Mutex<VecDeque<String>>>>,
    (code_label, parameters_label): (&str, &str),
    maps_processing_mode: Option<MapsProcessingMode>,
) {
    let mut in_sequence: bool = false;
    let mut lines: Vec<String> = Vec::with_capacity(4);
    let mut item_indices: Vec<usize> = Vec::with_capacity(4);

    for it in 0..list.len() {
        let code: u16 = list[it][code_label].as_u64().unwrap_log() as u16;

        let code: Code = if is_allowed_code(code) {
            let code: Code = unsafe { transmute(code) };

            if code == Code::DialogueStart && engine_type != EngineType::XP {
                Code::Bad
            } else {
                code
            }
        } else {
            Code::Bad
        };

        let write_string_literally: bool = if engine_type == EngineType::New {
            true
        } else {
            !match code {
                Code::ChoiceArray => list[it][parameters_label][0][0].is_object(),
                Code::Misc1 | Code::Misc2 | Code::Choice => list[it][parameters_label][1].is_object(),
                _ => list[it][parameters_label][0].is_object(),
            }
        };

        if in_sequence
            && (!matches!(code, Code::Dialogue | Code::DialogueStart)
                || (engine_type == EngineType::XP && code == Code::DialogueStart && !lines.is_empty()))
        {
            if !lines.is_empty() {
                let mut joined: String = lines.join("\n");

                if romanize {
                    joined = romanize_string(joined)
                }

                let translated: Option<String> = get_translated_parameter(
                    Code::Dialogue,
                    &joined,
                    if maps_processing_mode.is_some_and(|mode| mode == MapsProcessingMode::Preserve) {
                        None
                    } else {
                        Some(map)
                    },
                    deque.clone(),
                    game_type,
                    engine_type,
                );

                if let Some(translated) = translated {
                    let split_vec: Vec<&str> = translated.split('\n').collect();
                    let split_length: usize = split_vec.len();
                    let line_length: usize = lines.len();

                    for (i, &index) in item_indices.iter().enumerate() {
                        list[index][parameters_label][0] = if i < split_length {
                            if write_string_literally {
                                Value::from(split_vec[i])
                            } else {
                                json!({
                                    "__type": "bytes",
                                    "data": Array::from(split_vec[i].as_bytes())
                                })
                            }
                        } else {
                            Value::from_static_str(" ")
                        }
                    }

                    if split_length > line_length {
                        let remaining: String = split_vec[line_length - 1..].join("\n");
                        list[*unsafe { item_indices.last().unwrap_unchecked() }][parameters_label][0] =
                            Value::from(&remaining);
                    }
                }

                lines.clear();
                item_indices.clear();
            }

            in_sequence = false;
        }

        if code == Code::Bad {
            continue;
        }

        let value_i: usize = match code {
            Code::Misc1 | Code::Misc2 | Code::Choice => 1,
            _ => 0,
        };
        let value: &mut Value = &mut list[it][parameters_label][value_i];

        match code {
            Code::ChoiceArray => {
                for i in 0..value.as_array().unwrap_log().len() {
                    let subparameter_string: String = {
                        let mut buf: Vec<u8> = Vec::new();

                        let subparameter_string: &str = value[i]
                            .as_str()
                            .unwrap_or_else(|| match value[i].as_object() {
                                Some(obj) => {
                                    buf = get_object_data(obj);
                                    unsafe { std::str::from_utf8_unchecked(&buf) }
                                }
                                None => unreachable!(),
                            })
                            .trim();

                        if subparameter_string.is_empty() {
                            continue;
                        }

                        subparameter_string.to_owned()
                    };

                    process_parameter(
                        code,
                        subparameter_string,
                        Some(map),
                        None,
                        game_type,
                        engine_type,
                        romanize,
                        &mut value[i],
                    );
                }
            }
            _ => {
                let parameter_string: String = {
                    let mut buf: Vec<u8> = Vec::new();

                    let parameter_string: &str = value
                        .as_str()
                        .unwrap_or_else(|| match value.as_object() {
                            Some(obj) => {
                                buf = get_object_data(obj);
                                unsafe { std::str::from_utf8_unchecked(&buf) }
                            }
                            None => "",
                        })
                        .trim();

                    if code != Code::Credit && parameter_string.is_empty() {
                        continue;
                    }

                    parameter_string.to_owned()
                };

                if matches!(code, Code::Dialogue | Code::DialogueStart | Code::Credit) {
                    lines.push(parameter_string);
                    item_indices.push(it);
                    in_sequence = true;
                } else {
                    process_parameter(
                        code,
                        parameter_string,
                        Some(map),
                        None,
                        game_type,
                        engine_type,
                        romanize,
                        value,
                    );
                }
            }
        }
    }
}

/// Writes .txt files from maps folder back to their initial form.
/// # Parameters
/// * `maps_path` - path to the maps directory
/// * `original_path` - path to the original directory
/// * `output_path` - path to the output directory
/// * `maps_processing_mode` - how to deal with lines duplicates in maps
/// * `romanize` - if files were read with romanize, this option will romanize original game text to compare with parsed
/// * `logging` - whether to log or not
/// * `game_type` - game type for custom parsing
/// * `engine_type` - engine type for right files processing
#[inline(always)]
pub fn write_maps<P: AsRef<Path> + Sync>(
    maps_path: P,
    original_path: P,
    output_path: P,
    maps_processing_mode: MapsProcessingMode,
    romanize: bool,
    logging: bool,
    game_type: Option<GameType>,
    engine_type: EngineType,
) {
    let translation: String = read_to_string(maps_path.as_ref().join("maps.txt")).unwrap_log();

    let (names_map, translation_deque, translation_maps) = {
        // Allocated when maps processing mode is PRESERVE.
        let mut translation_deque: VecDeque<String> = VecDeque::new();
        // Default map for translation from the .txt file.
        let mut translation_map: StringHashMap = HashMap::with_hasher(HASHER);
        // A vec that holds translation maps. If maps processing mode is
        // DEFAULT, only ever holds one hashmap with all the translation lines.
        // If maps processing mode is SEPARATE, holds multiple hashmap, each
        // respective to a single map file.
        let mut translation_maps: HashMap<u16, StringHashMap, Xxh3DefaultBuilder> = HashMap::with_hasher(HASHER);
        // Always allocated.
        let mut names_map: StringHashMap = HashMap::with_hasher(HASHER);

        let mut map_number: u16 = 0;

        for (i, line) in translation.split('\n').enumerate() {
            if line.starts_with("<!-- Map") {
                if let Some((original, translated)) = line.split_once(LINES_SEPARATOR) {
                    if !original.ends_with(&(determine_extension(engine_type).to_owned() + " -->")) {
                        let map_display_name: &str = &original[17..original.len() - 4];
                        names_map.insert(map_display_name.trim_replace(), translated.trim_replace());
                    }

                    if maps_processing_mode == MapsProcessingMode::Separate && i > 0 {
                        translation_maps.insert(map_number, take(&mut translation_map));
                    }

                    map_number = parse_map_number(original);
                } else {
                    eprintln!("{COULD_NOT_SPLIT_LINE_MSG} ({line})\n{AT_POSITION_MSG} {i}", i = i + 1);
                }
            } else if !line.starts_with("<!--") {
                if let Some((original, translated)) = line.split_once(LINES_SEPARATOR) {
                    #[cfg(not(debug_assertions))]
                    if translated.is_empty() {
                        continue;
                    }

                    if maps_processing_mode == MapsProcessingMode::Preserve {
                        translation_deque.push_back(translated.replace(NEW_LINE, "\n").trim_replace());
                    } else {
                        translation_map.insert(
                            original.replace(NEW_LINE, "\n").trim_replace(),
                            translated.replace(NEW_LINE, "\n").trim_replace(),
                        );
                    }
                } else {
                    eprintln!(
                        "{COULD_NOT_SPLIT_LINE_MSG} ({line})\n{AT_POSITION_MSG} {i}\n{IN_FILE_MSG} maps.txt",
                        i = i + 1
                    );
                }
            }
        }

        if !translation_map.is_empty() {
            translation_maps.insert(map_number, translation_map);
        }

        (names_map, translation_deque, translation_maps)
    };

    let translation_deque_mutex: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(translation_deque));

    let (display_name_label, events_label, pages_label, list_label, code_label, parameters_label) =
        get_maps_labels(engine_type);

    let maps_obj_iter = read_dir(original_path)
        .unwrap_log()
        .par_bridge()
        .filter_map(|entry| filter_maps(entry, engine_type));

    maps_obj_iter.for_each(|(filename, mut obj)| {
        if let Some(mut display_name) = obj[display_name_label].as_str().map(str::to_owned) {
            if !display_name.is_empty() {
                if romanize {
                    display_name = romanize_string(display_name)
                }

                if let Some(location_name) = names_map.get(&display_name) {
                    obj[display_name_label] = Value::from(location_name);
                }
            }
        }

        let reserve_map: StringHashMap = StringHashMap::with_hasher(HASHER);

        let hashmap: &StringHashMap = if maps_processing_mode == MapsProcessingMode::Preserve {
            &reserve_map
        } else {
            let hashmap: &StringHashMap = if maps_processing_mode == MapsProcessingMode::Separate {
                unsafe {
                    let filename: &str = filename.split_once('.').unwrap_unchecked().0;
                    let map_number: u16 = parse_map_number(filename);
                    translation_maps.get(&map_number).unwrap_log()
                }
            } else {
                // translation_maps always have only one entry in this case
                unsafe { translation_maps.values().next().unwrap_unchecked() }
            };

            if hashmap.is_empty() {
                return;
            }

            hashmap
        };

        let events_arr: Box<dyn Iterator<Item = &mut Value> + Send> = if engine_type == EngineType::New {
            // Skipping first element in array as it is null
            Box::new(obj[events_label].as_array_mut().unwrap_log().iter_mut().skip(1))
        } else {
            Box::new(obj[events_label].as_object_mut().unwrap_log().iter_mut().map(|x| x.1))
        };

        let lines_deque_mutex: Arc<Mutex<VecDeque<String>>> = translation_deque_mutex.clone();
        events_arr.par_bridge().for_each(|event: &mut Value| {
            if event.is_null() {
                return;
            }

            let lines_deque_mutex: Arc<Mutex<VecDeque<String>>> = lines_deque_mutex.clone();
            event[pages_label]
                .as_array_mut()
                .unwrap_log()
                .par_iter_mut()
                .for_each(move |page: &mut Value| {
                    write_list(
                        page[list_label].as_array_mut().unwrap_log(),
                        romanize,
                        game_type,
                        engine_type,
                        hashmap,
                        Some(lines_deque_mutex.clone()),
                        (code_label, parameters_label),
                        Some(maps_processing_mode),
                    );
                });
        });

        let output_data: Vec<u8> = if engine_type == EngineType::New {
            unsafe { to_vec(&obj).unwrap_unchecked() }
        } else {
            dump(obj, Some(""))
        };

        write(output_path.as_ref().join(&filename), output_data).unwrap_log();

        if logging {
            println!("{WROTE_FILE_MSG} {filename}");
        }
    });
}

/// Writes .txt files from other folder back to their initial form.
/// # Parameters
/// * `other_path` - path to the other directory
/// * `original_path` - path to the original directory
/// * `output_path` - path to the output directory
/// * `romanize` - if files were read with romanize, this option will romanize original game text to compare with parsed
/// * `logging` - whether to log or not
/// * `game_type` - game type for custom parsing
/// * `engine_type` - engine type for right files processing
#[inline(always)]
pub fn write_other<P: AsRef<Path> + Sync>(
    other_path: P,
    original_path: P,
    output_path: P,
    romanize: bool,
    logging: bool,
    game_type: Option<GameType>,
    engine_type: EngineType,
) {
    let (
        name_label,
        nickname_label,
        description_label,
        message1_label,
        message2_label,
        message3_label,
        message4_label,
        note_label,
        pages_label,
        list_label,
        code_label,
        parameters_label,
    ) = get_other_labels(engine_type);

    let variable_tuples: Arc<[(&str, Variable); 8]> = Arc::new([
        (name_label, Variable::Name),
        (nickname_label, Variable::Nickname),
        (description_label, Variable::Description),
        (message1_label, Variable::Message1),
        (message2_label, Variable::Message2),
        (message3_label, Variable::Message3),
        (message4_label, Variable::Message4),
        (note_label, Variable::Note),
    ]);

    let other_obj_arr_iter = read_dir(original_path)
        .unwrap_log()
        .par_bridge()
        .filter_map(|entry| filter_other(entry, engine_type, game_type));

    other_obj_arr_iter.for_each(|(filename, mut obj_arr)| {
        let txt_filename: &str =
            &(unsafe { filename.rsplit_once('.').unwrap_unchecked() }.0.to_owned() + ".txt").to_lowercase();

        let translation_map: StringHashMap = {
            let translation: String = read_to_string(other_path.as_ref().join(txt_filename)).unwrap_log();
            HashMap::from_iter(parse_translation(&translation, txt_filename))
        };

        if translation_map.is_empty() {
            return;
        }

        // Other files except CommonEvents and Troops have the structure that consists
        // of name, nickname, description and note
        if !filename.starts_with("Co") && !filename.starts_with("Tr") {
            obj_arr
                .as_array_mut()
                .unwrap_log()
                .par_iter_mut()
                .skip(1) // Skipping first element in array as it is null
                .for_each(|obj: &mut Value| {
                    for (variable_label, variable_type) in variable_tuples.into_iter() {
                        if let Some(variable_str) = obj[variable_label].as_str() {
                            let mut variable_string: String = if variable_type == Variable::Note {
                                variable_str
                            } else {
                                variable_str.trim()
                            }
                            .to_owned();

                            if !variable_string.is_empty() {
                                if romanize {
                                    variable_string = romanize_string(variable_string)
                                }

                                variable_string = variable_string
                                    .split('\n')
                                    .map(str::trim)
                                    .collect::<Vec<_>>()
                                    .join("\n");

                                let note_text: Option<&str> = if game_type
                                    .is_some_and(|game_type: GameType| game_type != GameType::Termina)
                                    && variable_type != Variable::Description
                                {
                                    None
                                } else {
                                    match obj.get(unsafe { variable_tuples.last().unwrap_unchecked() }.0) {
                                        Some(value) => value.as_str(),
                                        None => None,
                                    }
                                };

                                let translated: Option<String> = get_translated_variable(
                                    variable_string,
                                    note_text,
                                    variable_type,
                                    &filename,
                                    &translation_map,
                                    game_type,
                                    engine_type,
                                );

                                if let Some(translated) = translated {
                                    obj[variable_label] = Value::from(&translated);
                                }
                            }
                        }
                    }
                });
        } else {
            // Other files have the structure somewhat similar to Maps files
            obj_arr
                .as_array_mut()
                .unwrap_log()
                .par_iter_mut()
                .skip(1) // Skipping first element in array as it is null
                .for_each(|obj: &mut Value| {
                    // CommonEvents doesn't have pages, so we can just check if it's Troops
                    let pages_length: usize = if filename.starts_with("Tr") {
                        obj[pages_label].as_array().unwrap_log().len()
                    } else {
                        1
                    };

                    for i in 0..pages_length {
                        // If element has pages, then we'll iterate over them
                        // Otherwise we'll just iterate over the list
                        let list: &mut Value = if pages_length != 1 {
                            &mut obj[pages_label][i][list_label]
                        } else {
                            &mut obj[list_label]
                        };

                        if let Some(list) = list.as_array_mut() {
                            write_list(
                                list,
                                romanize,
                                game_type,
                                engine_type,
                                &translation_map,
                                None,
                                (code_label, parameters_label),
                                None,
                            );
                        }
                    }
                });
        }

        let output_data: Vec<u8> = if engine_type == EngineType::New {
            unsafe { to_vec(&obj_arr).unwrap_unchecked() }
        } else {
            dump(obj_arr, Some(""))
        };

        write(output_path.as_ref().join(&filename), output_data).unwrap_log();

        if logging {
            println!("{WROTE_FILE_MSG} {filename}");
        }
    });
}

/// Writes system.txt file back to its initial form.
///
/// For inner code documentation, check `read_system` function.
/// # Parameters
/// * `system_file_path` - path to the original system file
/// * `other_path` - path to the other directory
/// * `output_path` - path to the output directory
/// * `romanize` - if files were read with romanize, this option will romanize original game text to compare with parsed
/// * `logging` - whether to log or not
/// * `engine_type` - engine type for right files processing
#[inline(always)]
pub fn write_system<P: AsRef<Path>>(
    system_file_path: P,
    other_path: P,
    output_path: P,
    romanize: bool,
    logging: bool,
    engine_type: EngineType,
) {
    let (translation_map, game_title): (StringHashMap, String) = {
        let translation: String = read_to_string(other_path.as_ref().join("system.txt")).unwrap_log();
        let game_title: String = translation.rsplit_once(LINES_SEPARATOR).unwrap_log().1.to_owned();
        (
            HashMap::from_iter(parse_translation(&translation, "system.txt")),
            game_title,
        )
    };

    if translation_map.is_empty() {
        return;
    }

    let replace_value = |value: &mut Value| {
        let mut buf: Vec<u8> = Vec::new();
        let str: &str = value
            .as_str()
            .unwrap_or_else(|| {
                if let Some(obj) = value.as_object() {
                    buf = get_object_data(obj);
                    unsafe { std::str::from_utf8_unchecked(&buf) }
                } else {
                    ""
                }
            })
            .trim();

        if !str.is_empty() {
            let mut string: String = str.to_owned();

            if romanize {
                string = romanize_string(string);
            }

            if let Some(translated) = translation_map.get(&string) {
                *value = if engine_type == EngineType::New {
                    Value::from(translated)
                } else {
                    json!({"__type": "bytes", "data": Array::from(translated.as_bytes())})
                };
            }
        }
    };

    let (armor_types_label, elements_label, skill_types_label, terms_label, weapon_types_label, game_title_label) =
        get_system_labels(engine_type);

    let mut obj: Value = if engine_type == EngineType::New {
        from_str(&read_to_string_without_bom(&system_file_path).unwrap_log()).unwrap_log()
    } else {
        load(&read(&system_file_path).unwrap_log(), Some(StringMode::UTF8), Some("")).unwrap_log()
    };

    for label in [
        armor_types_label,
        elements_label,
        skill_types_label,
        weapon_types_label,
        "equipTypes",
    ] {
        if let Some(arr) = obj[label].as_array_mut() {
            arr.iter_mut().for_each(replace_value);
        }
    }

    obj[terms_label]
        .as_object_mut()
        .unwrap_log()
        .iter_mut()
        .for_each(|(key, value): (&str, &mut Value)| {
            if engine_type != EngineType::New && !key.starts_with("__symbol__") {
                return;
            }

            if key != "messages" {
                if let Some(arr) = value.as_array_mut() {
                    arr.par_iter_mut().for_each(replace_value);
                } else if (value.is_object() && value["__type"].as_str().is_some_and(|x| x == "bytes"))
                    || value.is_str()
                {
                    replace_value(value)
                }
            } else {
                if !value.is_object() {
                    return;
                }

                value
                    .as_object_mut()
                    .unwrap_log()
                    .iter_mut()
                    .for_each(|(_, value)| replace_value(value));
            }
        });

    if engine_type != EngineType::New {
        replace_value(&mut obj["__symbol__currency_unit"]);
    }

    if !game_title.is_empty() {
        obj[game_title_label] = Value::from(&game_title);
    }

    let output_data: Vec<u8> = if engine_type == EngineType::New {
        unsafe { to_vec(&obj).unwrap_unchecked() }
    } else {
        dump(obj, Some(""))
    };

    let filename: &OsStr = unsafe { system_file_path.as_ref().file_name().unwrap_unchecked() };

    write(output_path.as_ref().join(filename), output_data).unwrap_log();

    if logging {
        println!("{WROTE_FILE_MSG} {filename:?}");
    }
}

/// Writes plugins.txt file back to its initial form.
/// # Parameters
/// * `plugins_file_path` - path to the original plugins file
/// * `plugins_path` - path to the plugins directory
/// * `output_path` - path to the output directory
/// * `logging` - whether to log or not
/// * `romanize` - if files were read with romanize, this option will romanize original game text to compare with parsed
#[inline(always)]
pub fn write_plugins<P: AsRef<Path>>(
    plugins_file_path: P,
    plugins_path: P,
    output_path: P,
    logging: bool,
    romanize: bool,
) {
    let mut translation_map: VecDeque<(String, String)> = {
        let translation: String = read_to_string(plugins_path.as_ref().join("plugins.txt")).unwrap_log();
        VecDeque::from_iter(parse_translation(&translation, "plugins.txt"))
    };

    let translation_set: HashSet<String, Xxh3DefaultBuilder> =
        HashSet::from_iter(translation_map.iter().map(|x| x.0.to_owned()));

    let plugins_content: String = read_to_string(plugins_file_path.as_ref()).unwrap_log();

    let plugins_object: &str = plugins_content
        .split_once('=')
        .unwrap_log()
        .1
        .trim_end_matches([';', '\n']);

    let mut plugins_json: Value = from_str(plugins_object).unwrap_log();

    traverse_json(
        None,
        &mut plugins_json,
        &mut None,
        &mut Some(&mut translation_map),
        &Some(&translation_set),
        true,
        romanize,
        ProcessingMode::Default,
    );

    write(
        output_path.as_ref().join("plugins.js"),
        String::from("var $plugins =\n") + unsafe { &to_string(&plugins_json).unwrap_unchecked() },
    )
    .unwrap_log();

    if logging {
        println!("{WROTE_FILE_MSG} plugins.js");
    }
}

/// Writes scripts.txt file back to its initial form.
///
/// For inner code documentation, check read_system function.
/// # Parameters
/// * `scripts_file_path` - path to the original system file
/// * `other_path` - path to the other directory
/// * `output_path` - path to the output directory
/// * `romanize` - if files were read with romanize, this option will romanize original game text to compare with parsed
/// * `logging` - whether to log or not
/// * `engine_type` - engine type for right files processing
pub fn write_scripts<P: AsRef<Path>>(
    scripts_file_path: P,
    other_path: P,
    output_path: P,
    romanize: bool,
    logging: bool,
    engine_type: EngineType,
) {
    let translation_map: StringHashMap = {
        let translation: String = read_to_string(other_path.as_ref().join("scripts.txt")).unwrap_log();
        StringHashMap::from_iter(parse_translation(&translation, "scripts.txt"))
    };

    if translation_map.is_empty() {
        return;
    }

    let mut script_entries: Value =
        load(&read(&scripts_file_path).unwrap_log(), Some(StringMode::Binary), None).unwrap_log();

    script_entries
        .as_array_mut()
        .unwrap_log()
        .iter_mut()
        .for_each(|script: &mut Value| {
            let data: Vec<u8> = from_value(&script.as_array().unwrap_log()[2]["data"]).unwrap_log();

            let mut inflated: Vec<u8> = Vec::new();
            ZlibDecoder::new(&*data).read_to_end(&mut inflated).unwrap_log();

            let mut code: String = String::new();

            for encoding in ENCODINGS {
                let (cow, _, had_errors) = encoding.decode(&inflated);

                if !had_errors {
                    code = cow.into_owned();
                    break;
                }
            }

            let (extracted_strings, ranges) = extract_strings(&code, true);

            for (mut extracted, range) in extracted_strings.into_iter().zip(ranges).rev() {
                if extracted.is_empty() {
                    continue;
                }

                if romanize {
                    extracted = romanize_string(extracted);
                }

                if let Some(translated) = translation_map.get(&extracted) {
                    code.replace_range(range, translated);
                }
            }

            let mut buf: Vec<u8> = Vec::new();

            ZlibEncoder::new(&mut buf, Compression::new(6))
                .write_all(code.as_bytes())
                .unwrap_log();

            if let Some(obj) = script[2].as_object_mut() {
                obj["data"] = Array::from(buf).into()
            };
        });

    write(
        output_path
            .as_ref()
            .join(String::from("Scripts") + determine_extension(engine_type)),
        dump(script_entries, None),
    )
    .unwrap_log();

    if logging {
        println!("{WROTE_FILE_MSG} {:?}", unsafe {
            scripts_file_path.as_ref().file_name().unwrap_unchecked()
        });
    }
}
