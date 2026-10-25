use anyhow::Result;
use retour::static_detour;
use std::ffi::c_char;

use crate::config::CONFIG;
use crate::cxxstring::CxxString;
use crate::dictionary;
use crate::utils;

use r#macro::hook;

#[cfg(target_os = "linux")]
#[static_init::dynamic]
static ENABLER: usize = unsafe {
  match CONFIG.symbol.is_some() {
    true => {
      utils::symbol_handle_self::<*const i64>(&CONFIG.symbol.as_ref().unwrap().enabler.as_ref().unwrap()[1]) as usize
    }
    false => 0 as usize,
  }
};

use std::sync::atomic::{AtomicBool, Ordering};

/// Установлены ли необязательные перехваты. Неустановленный перехват
/// нельзя включать и выключать: он вернёт ошибку и сорвёт включение
/// остальных (горячая клавиша Ctrl+F2).
static ADDST_FLAG_ATTACHED: AtomicBool = AtomicBool::new(false);
static CSTR_PLUS_ATTACHED: AtomicBool = AtomicBool::new(false);
static ADDCOLOREDST_ATTACHED: AtomicBool = AtomicBool::new(false);

/// addst_flag рисует вкладки и кнопки. dfint на Windows его отключил
/// (март 2024, без пояснения); проверка на 52.05 показала, что он
/// работает. Аварийный выключатель — строка addst_flag_off в
/// dfint-data/ai/experiments.txt.
fn addst_flag_wanted() -> bool {
  if cfg!(target_os = "linux") {
    return true;
  }
  let has_offset = CONFIG.offset.as_ref().map_or(false, |o| o.addst_flag.is_some());
  has_offset && !crate::ai::EXPERIMENTS.addst_flag_off
}

// ==================== ПЕРЕХВАТЫ СВЕРХ DFINT ====================
//
// Адреса найдены разбором Dwarf Fortress.exe 52.05 (steam, win64), в
// списках dfint их нет. Перехват ставится только на этой версии и только
// если первые 16 байт функции совпадают с увиденными при разборе — на
// любой другой сборке он просто не встанет.

const EXTRA_CHECKSUM: u32 = 0x68D6_4CE7;

/// std::string operator+(const char*, const std::string&): склейка
/// строки-константы с текстом. Так собираются, например, черты характера
/// («is not a private person and freely shares details of » + имя);
/// перевод в словаре dfint есть, но до этой функции он не доходил.
const CSTR_PLUS_STRING: usize = 0x148d10;
const CSTR_PLUS_STRING_PROLOGUE: [u8; 16] = [
  0x40, 0x53, 0x55, 0x56, 0x41, 0x56, 0x41, 0x57, 0x48, 0x83, 0xec, 0x30, 0x48, 0x8b, 0xf2, 0x4d,
];

/// graphicst::addcoloredst(const char* text, const char* colors): вывод
/// строки с отдельным цветом каждой буквы — так игра рисует мысли и
/// характер дварфа. Цвет буквы: биты 0–2 — цвет текста, 3–5 — фона,
/// 6 — яркость. Аварийный выключатель — строка colored_off.
const ADDCOLOREDST: usize = 0xa99320;
const ADDCOLOREDST_PROLOGUE: [u8; 16] = [
  0x48, 0x89, 0x5c, 0x24, 0x08, 0x48, 0x89, 0x6c, 0x24, 0x10, 0x48, 0x89, 0x74, 0x24, 0x18, 0x57,
];

fn extra_ok(offset: usize, prologue: &[u8; 16]) -> bool {
  if cfg!(not(target_os = "windows")) || CONFIG.offset_metadata.checksum != EXTRA_CHECKSUM {
    return false;
  }
  let bytes = unsafe { std::slice::from_raw_parts(utils::address(offset) as *const u8, 16) };
  bytes == prologue
}

pub unsafe fn attach_all() -> Result<()> {
  if CONFIG.settings.enable_translation {
    attach_string_copy_n()?;
    attach_string_append_n()?;
    attach_std_string_ctor()?;
    attach_std_string_append()?;
    attach_std_string_assign()?;
    attach_addst()?;
    attach_addst_top()?;
    if addst_flag_wanted() {
      attach_addst_flag()?;
      ADDST_FLAG_ATTACHED.store(true, Ordering::Relaxed);
      log::info!("addst_flag hook attached");
    }
    if !crate::ai::EXPERIMENTS.cstr_plus_off && extra_ok(CSTR_PLUS_STRING, &CSTR_PLUS_STRING_PROLOGUE) {
      attach_cstr_plus_string()?;
      CSTR_PLUS_ATTACHED.store(true, Ordering::Relaxed);
      log::info!("cstr_plus_string hook attached");
    }
    if !crate::ai::EXPERIMENTS.colored_off && extra_ok(ADDCOLOREDST, &ADDCOLOREDST_PROLOGUE) {
      attach_addcoloredst()?;
      ADDCOLOREDST_ATTACHED.store(true, Ordering::Relaxed);
      log::info!("addcoloredst hook attached");
    }
  }
  if CONFIG.settings.enable_search && CONFIG.encoding.parsed {
    attach_standardstringentry()?;
    attach_simplify_string()?;
    attach_upper_case_string()?;
    attach_lower_case_string()?;
    attach_capitalize_string_words()?;
    attach_capitalize_string_first_word()?;
  }
  Ok(())
}

pub unsafe fn enable_translation() -> Result<()> {
  enable_string_copy_n()?;
  enable_string_append_n()?;
  enable_std_string_ctor()?;
  enable_std_string_append()?;
  enable_std_string_assign()?;
  enable_addst()?;
  enable_addst_top()?;
  if ADDST_FLAG_ATTACHED.load(Ordering::Relaxed) {
    enable_addst_flag()?;
  }
  if CSTR_PLUS_ATTACHED.load(Ordering::Relaxed) {
    enable_cstr_plus_string()?;
  }
  if ADDCOLOREDST_ATTACHED.load(Ordering::Relaxed) {
    enable_addcoloredst()?;
  }
  Ok(())
}

pub unsafe fn enable_search() -> Result<()> {
  enable_standardstringentry()?;
  enable_simplify_string()?;
  enable_upper_case_string()?;
  enable_lower_case_string()?;
  enable_capitalize_string_words()?;
  enable_capitalize_string_first_word()?;
  Ok(())
}

pub unsafe fn enable_all() -> Result<()> {
  enable_translation()?;
  enable_search()?;
  Ok(())
}

pub unsafe fn disable_translation() -> Result<()> {
  disable_string_copy_n()?;
  disable_string_append_n()?;
  disable_std_string_ctor()?;
  disable_std_string_append()?;
  disable_std_string_assign()?;
  disable_addst()?;
  disable_addst_top()?;
  if ADDST_FLAG_ATTACHED.load(Ordering::Relaxed) {
    disable_addst_flag()?;
  }
  if CSTR_PLUS_ATTACHED.load(Ordering::Relaxed) {
    disable_cstr_plus_string()?;
  }
  if ADDCOLOREDST_ATTACHED.load(Ordering::Relaxed) {
    disable_addcoloredst()?;
  }
  Ok(())
}

pub unsafe fn disable_search() -> Result<()> {
  disable_standardstringentry()?;
  disable_simplify_string()?;
  disable_upper_case_string()?;
  disable_lower_case_string()?;
  disable_capitalize_string_words()?;
  disable_capitalize_string_first_word()?;
  Ok(())
}

pub unsafe fn disable_all() -> Result<()> {
  disable_translation()?;
  disable_search()?;
  Ok(())
}

#[cfg_attr(target_os = "windows", hook(by_offset))]
#[cfg_attr(target_os = "linux", hook(bypass))]
fn string_copy_n(dst: *mut c_char, src: *const u8, size: usize) -> *mut c_char {
  unsafe {
    match (std::slice::from_raw_parts(src, size), size > 1) {
      (value, true) => match dictionary::lookup(value) {
        Some(translate) => {
          let (ptr, len, _) = translate.to_owned().into_raw_parts();
          original!(dst, ptr, len - 1)
        }
        _ => original!(dst, src, size),
      },
      (_, _) => original!(dst, src, size),
    }
  }
}

#[cfg_attr(target_os = "windows", hook(by_offset))]
#[cfg_attr(target_os = "linux", hook(bypass))]
fn string_append_n(dst: *mut c_char, src: *const u8, size: usize) -> *mut c_char {
  unsafe {
    match (std::slice::from_raw_parts(src, size), size > 1) {
      (value, true) => match dictionary::lookup(value) {
        Some(translate) => {
          let (ptr, len, _) = translate.to_owned().into_raw_parts();
          original!(dst, ptr, len - 1)
        }
        _ => original!(dst, src, size),
      },
      (_, _) => original!(dst, src, size),
    }
  }
}

#[cfg_attr(target_os = "windows", hook(by_offset))]
#[cfg_attr(target_os = "linux", hook(bypass))]
fn std_string_ctor(dst: *const u8, src: *const u8, size: usize) -> *const u8 {
  unsafe {
    match (std::slice::from_raw_parts(src, size), size > 1) {
      (value, true) => match dictionary::lookup(value) {
        Some(translate) => {
          let (ptr, len, _) = translate.to_owned().into_raw_parts();
          original!(dst, ptr, len - 1)
        }
        _ => original!(dst, src, size),
      },
      (_, _) => original!(dst, src, size),
    }
  }
}

#[cfg_attr(target_os = "windows", hook(bypass))]
#[cfg_attr(target_os = "linux", hook(by_symbol))]
fn std_string_append(dst: *const u8, src: *const u8) -> *const u8 {
  unsafe {
    match std::ffi::CStr::from_ptr(src as *const c_char).to_bytes() {
      (value) => match dictionary::lookup(value) {
        Some(translate) => {
          let (ptr, _, _) = translate.to_owned().into_raw_parts();
          original!(dst, ptr)
        }
        _ => original!(dst, src),
      },
      _ => original!(dst, src),
    }
  }
}

#[cfg_attr(target_os = "windows", hook(bypass))]
#[cfg_attr(target_os = "linux", hook(by_symbol))]
fn std_string_assign(dst: *const u8, src: *const u8) -> *const u8 {
  unsafe {
    match std::ffi::CStr::from_ptr(src as *const c_char).to_bytes() {
      (value) => match dictionary::lookup(value) {
        Some(translate) => {
          let (ptr, _, _) = translate.to_owned().into_raw_parts();
          original!(dst, ptr)
        }
        _ => original!(dst, src),
      },
      _ => original!(dst, src),
    }
  }
}

/// addst выводит строку с позиции курсора. Кроме обычного поиска
/// перевода здесь работают фразы, которые игра рисует по слову (flow.rs):
/// слово либо выводится как обычно (со сдвигом временной раскладки),
/// либо вместо него рисуются куски перевода всей фразы.
#[cfg_attr(target_os = "windows", hook(by_offset))]
#[cfg_attr(target_os = "linux", hook(by_symbol))]
fn addst(gps: usize, src: *const u8, justify: u8, space: u32) {
  unsafe {
    let s = CxxString::from_ptr(src);
    let converted = s.to_bytes_without_nul();
    crate::ai::probe(b'A', gps, converted);
    draw_text(gps, src, converted, *crate::align::ADDST, space, &|p| original!(gps, p, justify, space));
  }
}

/// Вывод надписи через addst или addst_top (верхний слой: окна обучения,
/// подсказки поверх карты). Одинаково для обоих: фразы, которые игра
/// рисует по слову (flow.rs), выравнивание (align.rs), место (fit.rs).
/// draw — исходная функция игры с уже подставленными остальными
/// аргументами.
///
/// space — последний аргумент addst: если он не ноль, игра сама сокращает
/// надпись до space знаков (так рисуется колонка навыка на странице
/// «Труд»: «Professional Miner» — ровно 18). Русская надпись длиннее
/// обрезается игрой, и это место уходит в fit.rs.
unsafe fn draw_text(gps: usize, src: *const u8, converted: &[u8], target: usize, space: u32, draw: &dyn Fn(*const u8)) {
  unsafe {
    let plan = crate::flow::begin(gps, converted, 0);
    let (shift, quiet, in_flow) = match plan {
      crate::flow::Plan::Pieces(pieces) => {
        // Куски перевода фразы — каждый в свою клетку. Буфер живёт до
        // конца вызова: игра копирует текст на экран и указатель не хранит.
        // Не меньше 16 байт: короткую строку конструктор читает целым буфером
        let (x, y) = crate::ai::cursor(gps);
        let len = converted.len() as i32;
        for (px, py, text) in pieces {
          let n = text.len();
          let mut buf = text;
          buf.resize(n.max(16) + 1, 0);
          let mut cxxstr = CxxString::new(buf.as_mut_ptr(), n);
          #[cfg(target_os = "linux")]
          {
            if cxxstr.len < 16 {
              cxxstr.ptr = cxxstr.sso.buf.as_mut_ptr();
            }
          }
          crate::ai::set_cursor(gps, px, py);
          draw(cxxstr.as_ptr());
        }
        // Курсор — как после вывода самого слова
        crate::ai::set_cursor(gps, x + len, y);
        return;
      }
      crate::flow::Plan::Normal { shift, quiet, flow } => (shift, quiet, flow),
    };

    // Поиск перевода — до сдвига: сборка абзацев смотрит на позицию курсора.
    // Слово внутри фразы ищется без записи в журнал: его переведут фразой
    let translated = if quiet {
      dictionary::lookup_all(converted)
    } else {
      dictionary::lookup_screen(gps, converted)
    };
    // Надпись по центру или по правому краю: игра посчитала столбец по
    // английской длине — сдвигаем на разницу (align.rs)
    if !in_flow {
      realign(gps, target, converted, translated.as_ref());
    }
    // Место для надписи: обрезка, если она налезает на соседа справа
    let translated = fitted(gps, converted, translated, in_flow, space);
    if shift != 0 {
      let (x, y) = crate::ai::cursor(gps);
      crate::ai::set_cursor(gps, x + shift, y);
    }
    match translated {
      Some(translate) => {
        crate::flow::drawn(converted.len(), translate.len().saturating_sub(1));
        let n = translate.len().saturating_sub(1);
        let mut buf = translate;
        buf.resize(n.max(16) + 1, 0);
        let mut cxxstr = CxxString::new(buf.as_mut_ptr(), n);
        #[cfg(target_os = "linux")]
        {
          if cxxstr.len < 16 {
            cxxstr.ptr = cxxstr.sso.buf.as_mut_ptr();
          }
        }
        draw(cxxstr.as_ptr())
      }
      None => draw(src),
    }
  }
}

/// Сдвиг курсора для надписи, которую игра выровняла по английской длине.
fn realign(gps: usize, target: usize, key: &[u8], translated: Option<&Vec<u8>>) {
  let dx = crate::align::for_draw(target, key, translated);
  if dx != 0 {
    let (x, y) = crate::ai::cursor(gps);
    crate::ai::set_cursor(gps, x + dx, y);
  }
}

/// Надпись с учётом места на экране (fit.rs): перевод или сама надпись,
/// обрезанные за клетку до соседа справа. None — выводить как есть.
/// Значение, как и в словаре, заканчивается нулём.
fn fitted(gps: usize, key: &[u8], translated: Option<Vec<u8>>, in_flow: bool, space: u32) -> Option<Vec<u8>> {
  let drawn = translated.as_ref().map(|t| &t[..t.len().saturating_sub(1)]);
  match crate::fit::settle(gps, key, drawn, in_flow, space) {
    Some(room) => {
      // Не помещается — сократить словами («Проф. шахтёр»), а не обрубить
      let text = drawn.unwrap_or(key);
      let mut cut = crate::fit::abbreviate(text, room);
      cut.push(0);
      Some(cut)
    }
    None => translated,
  }
}

#[cfg_attr(target_os = "windows", hook(by_offset))]
#[cfg_attr(target_os = "linux", hook(by_symbol))]
fn addst_top(gps: usize, src: *const u8, justify: u8, space: u32) {
  unsafe {
    let s = CxxString::from_ptr(src);
    let converted = s.to_bytes_without_nul();
    crate::ai::probe(b'T', gps, converted);
    draw_text(gps, src, converted, *crate::align::ADDST_TOP, space, &|p| original!(gps, p, justify, space));
  }
}

// На Windows dfint этот перехват отключил (март 2024, без пояснения).
// Здесь он включён — см. addst_flag_wanted
#[cfg_attr(target_os = "windows", hook(by_offset))]
#[cfg_attr(target_os = "linux", hook(by_symbol))]
fn addst_flag(gps: usize, src: *const u8, a3: usize, a4: usize, flag: u32) {
  unsafe {
    let s = CxxString::from_ptr(src);
    match s.to_bytes_without_nul() {
      converted => match {
        crate::ai::probe(b'F', gps, converted);
        // Четвёртый аргумент addst_flag — как space у addst: игра сама
        // сокращает надпись до этого числа знаков (0xa9943c), так рисуется
        // колонка навыка на странице «Труд»
        fitted(gps, converted, dictionary::lookup_screen(gps, converted), false, a4 as u32)
      } {
        Some(translate) => {
          let (ptr, len, _) = translate.to_owned().into_raw_parts();
          let mut cxxstr = CxxString::new(ptr, len - 1);
          #[cfg(target_os = "linux")]
          {
            if cxxstr.len < 16 {
              cxxstr.ptr = cxxstr.sso.buf.as_mut_ptr();
            }
          }
          original!(gps, cxxstr.as_ptr(), a3, a4, flag)
        }
        _ => original!(gps, src, a3, a4, flag),
      },
      _ => original!(gps, src, a3, a4, flag),
    }
  }
}

#[cfg_attr(target_os = "windows", hook(offset = "148d10"))]
#[cfg_attr(target_os = "linux", hook(bypass))]
fn cstr_plus_string(result: usize, lhs: *const c_char, rhs: usize) -> usize {
  unsafe {
    if lhs.is_null() {
      return original!(result, lhs, rhs);
    }
    let bytes = std::ffi::CStr::from_ptr(lhs).to_bytes();
    // Только фразы, только словарь сообщества и без записи в журнал:
    // через эту склейку игра собирает и служебные имена — см.
    // concat_fragment_allowed
    if !crate::ai::concat_fragment_allowed(bytes) {
      return original!(result, lhs, rhs);
    }
    match dictionary::lookup(bytes) {
      Some(translate) => {
        // Значение словаря уже заканчивается нулём — это готовая C-строка.
        // Склейка копирует её в результат, поэтому буфер освобождается
        // после вызова — без утечки
        original!(result, translate.as_ptr() as *const c_char, rhs)
      }
      None => original!(result, lhs, rhs),
    }
  }
}

#[cfg_attr(target_os = "windows", hook(offset = "a99320"))]
#[cfg_attr(target_os = "linux", hook(bypass))]
fn addcoloredst(gps: usize, text: *const c_char, colors: *const u8) {
  unsafe {
    if text.is_null() || colors.is_null() {
      return original!(gps, text, colors);
    }
    let bytes = std::ffi::CStr::from_ptr(text).to_bytes();
    crate::ai::probe(b'C', gps, bytes);
    let original_colors = std::slice::from_raw_parts(colors, bytes.len());
    match crate::ai::colored(gps, bytes, original_colors) {
      // Функция читает текст и цвета только во время вызова, поэтому
      // буферы живут до его конца и освобождаются — без утечки
      Some((new_text, new_colors)) => original!(gps, new_text.as_ptr() as *const c_char, new_colors.as_ptr()),
      None => original!(gps, text, colors),
    }
  }
}

#[non_exhaustive]
struct StringEntry;

#[allow(dead_code)]
impl StringEntry {
  pub const LETTERS: u8 = 1;
  pub const SPACE: u8 = 2;
  pub const NUMBERS: u8 = 4;
  pub const CAPS: u8 = 8;
  pub const SYMBOLS: u8 = 16;
  pub const STRINGENTRY_FILENAME: u8 = 32;
}

#[cfg_attr(target_os = "windows", hook(by_offset))]
#[cfg_attr(target_os = "linux", hook(by_symbol))]
fn standardstringentry(src: *const u8, maxlen: usize, flag: u8, events_ptr: *const u8, utf: *const u32) -> bool {
  unsafe {
    let utf_a = std::slice::from_raw_parts_mut(utf as *mut u32, 8);
    #[cfg(target_os = "linux")]
    {
      let utf_a = std::slice::from_raw_parts_mut(
        (*ENABLER + &CONFIG.offset.as_ref().unwrap().utf_input.unwrap()) as *mut u32,
        8,
      );
    }

    let content = CxxString::from_ptr(src);
    let mut any_valid = false;

    for i in 0..8 {
      if utf_a[i] == 0 || utf_a[i] == 10 || content.size() >= maxlen {
        break;
      }

      if utf_a[i] > 122 && CONFIG.encoding.utf.contains_key(&utf_a[i]) {
        let entry = CONFIG.encoding.utf[&utf_a[i]];
        utf_a[i] = match (flag & StringEntry::CAPS) > 0 {
          true => capitalize(entry),
          false => entry,
        } as u32;

        if content.size() < maxlen {
          content.push_back(utf_a[i] as u8);
          any_valid = true;
        }
      }
    }

    any_valid || original!(content.as_ptr(), maxlen, flag, events_ptr, utf_a.as_ptr())
  }
}

fn capitalize(symbol: u8) -> u8 {
  CONFIG.encoding.capitalize[symbol as usize]
}

#[allow(dead_code)]
fn lowercast(symbol: u8) -> u8 {
  CONFIG.encoding.lowercast[symbol as usize]
}

#[cfg_attr(target_os = "windows", hook(by_offset))]
#[cfg_attr(target_os = "linux", hook(by_symbol))]
fn simplify_string(src: *const u8) {
  unsafe {
    let mut content = CxxString::from_ptr(src);
    for i in 0..content.len {
      content[i] = CONFIG.encoding.simplify[content[i] as usize];
    }
  }
}

#[cfg_attr(target_os = "windows", hook(by_offset))]
#[cfg_attr(target_os = "linux", hook(by_symbol))]
fn upper_case_string(src: *const u8) {
  unsafe {
    let mut content = CxxString::from_ptr(src);
    for i in 0..content.len {
      content[i] = CONFIG.encoding.uppercase[content[i] as usize]
    }
  }
}

#[cfg_attr(target_os = "windows", hook(by_offset))]
#[cfg_attr(target_os = "linux", hook(by_symbol))]
fn lower_case_string(src: *const u8) {
  unsafe {
    let mut content = CxxString::from_ptr(src);
    for i in 0..content.len {
      content[i] = CONFIG.encoding.lowercase[content[i] as usize]
    }
  }
}

#[cfg_attr(target_os = "windows", hook(by_offset))]
#[cfg_attr(target_os = "linux", hook(by_symbol))]
fn capitalize_string_words(src: *const u8) {
  unsafe {
    let mut bracket_count: i32 = 0;
    let mut content = CxxString::from_ptr(src);
    for i in 0..content.len {
      match content[i] {
        91 => {
          bracket_count += 1;
          continue;
        }
        93 => {
          bracket_count -= 1;
          continue;
        }
        _ => (),
      };
      if bracket_count > 0 {
        continue;
      }
      let mut conf = false;
      if (i > 0 && content[i - 1] == 32 || content[i - 1] == 34)
        || (i >= 2 && content[i - 1] == 39 && (content[i - 2] == 32 || content[i - 2] == 44))
      {
        conf = true;
      }
      if i == 0 || conf {
        content[i] = CONFIG.encoding.uppercase[content[i] as usize];
      }
    }
  }
}

#[cfg_attr(target_os = "windows", hook(by_offset))]
#[cfg_attr(target_os = "linux", hook(by_symbol))]
fn capitalize_string_first_word(src: *const u8) {
  unsafe {
    let mut bracket_count: i32 = 0;
    let mut content = CxxString::from_ptr(src);
    for i in 0..content.len {
      match content[i] {
        91 => {
          bracket_count += 1;
          continue;
        }
        93 => {
          bracket_count -= 1;
          continue;
        }
        _ => (),
      };
      if bracket_count > 0 {
        continue;
      }
      let mut conf = false;
      if (i > 0 && content[i - 1] == 32 || content[i - 1] == 34)
        || (i >= 2 && content[i - 1] == 39 && (content[i - 2] == 32 || content[i - 2] == 44))
      {
        conf = true;
      }
      if i == 0 || conf {
        content[i] = CONFIG.encoding.uppercase[content[i] as usize];
        if content[i] != 32 && content[i] != 34 {
          return;
        }
      }
    }
  }
}
