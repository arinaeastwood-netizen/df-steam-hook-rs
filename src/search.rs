//! Русский поиск в списках игры.
//!
//! Названия в памяти игры английские: перевод появляется только в момент
//! рисования. Поиск игры устроен так (разбор Dwarf Fortress.exe 52.05):
//! запрос из поля и каждое название проходят через simplify_string —
//! строчные буквы, без значков, — и запрос ищется внутри названия.
//! Английское «bed» русский запрос «кро» не найдёт никогда.
//!
//! Поэтому, пока в поле поиска есть русские буквы (textinput.rs), хук
//! дописывает к английскому названию в simplify_string его перевод:
//! «bed» → «bed␁ кровать». Запрос «кро» находится в переводе, английский
//! запрос — в оригинале. Перевод отделён байтом 0x01 и пробелом: 0x01
//! меньше любой буквы, поэтому сортировка по таким строкам остаётся
//! английской, а пробел делает перевод отдельным словом — для экранов,
//! где игра ищет запрос в начале слов.
//!
//! Перевод берётся из всех словарей, которыми хук рисует текст:
//! нейросетевого, сообщества и базового. Если названия целиком нет,
//! переводятся отдельные слова («oak bed» → «дуб», «кровать»). Название,
//! которое ни разу не попадало на экран и не переведено, найдётся только
//! по-английски.
//!
//! Запись в журнал отсюда не идёт: это не вывод на экран.
//!
//! Аварийный выключатель — строка search_ru_off в dfint-data/ai/experiments.txt.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::ai;
use crate::config::CONFIG;
use crate::dictionary;

/// Разделитель английского названия и его перевода.
pub const SEP: [u8; 2] = [0x01, b' '];
/// Длиннее этого — не название, а текст; такие строки не дополняются.
const MAX_KEY: usize = 160;
/// Предел дописываемого перевода.
const MAX_EXTRA: usize = 240;
/// Предел кэша переводов. Больше — кэш начинается заново.
const MAX_CACHE: usize = 50_000;
/// Сколько первых русских ключей записывается в лог.
const LOG_LIMIT: usize = 20;

struct Cache {
  generation: usize,
  map: HashMap<Vec<u8>, Option<Vec<u8>>>,
}

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| {
  Mutex::new(Cache {
    generation: 0,
    map: HashMap::new(),
  })
});
static LOGGED: AtomicUsize = AtomicUsize::new(0);

/// Русский ключ для английского названия: байт-разделитель и перевод,
/// уже упрощённый (строчные буквы) — так же, как игра упростит запрос.
/// None — ничего не дописывать.
pub fn russian_key(original: &[u8]) -> Option<Vec<u8>> {
  if ai::EXPERIMENTS.search_ru_off || !crate::textinput::russian_search_active() {
    return None;
  }
  if original.len() < 2 || original.len() > MAX_KEY {
    return None;
  }
  // Уже русское или служебное — не трогаем
  if original.iter().any(|&b| b >= 0x80 || b < 0x20) {
    return None;
  }
  if !original.iter().any(|b| b.is_ascii_alphabetic()) {
    return None;
  }

  let generation = ai::generation();
  if let Ok(mut cache) = CACHE.try_lock() {
    if cache.generation != generation {
      cache.map.clear();
      cache.generation = generation;
    }
    if let Some(value) = cache.map.get(original) {
      return value.clone();
    }
  }

  let value = build(original);

  if let Ok(mut cache) = CACHE.try_lock() {
    if cache.generation == generation {
      if cache.map.len() >= MAX_CACHE {
        cache.map.clear();
      }
      cache.map.insert(original.to_vec(), value.clone());
    }
  }

  if let Some(extra) = &value {
    if LOGGED.fetch_add(1, Ordering::Relaxed) < LOG_LIMIT {
      log::info!(
        "search: \"{}\" + {} bytes of russian key",
        String::from_utf8_lossy(original),
        extra.len()
      );
    }
  }
  value
}

fn build(original: &[u8]) -> Option<Vec<u8>> {
  let mut parts = translations(original);
  if parts.is_empty() {
    // Целиком перевода нет — по словам
    for word in original.split(|b| !(b.is_ascii_alphanumeric() || *b == b'-' || *b == b'\'')) {
      if word.len() < 2 || !word.iter().any(|b| b.is_ascii_alphabetic()) {
        continue;
      }
      for t in translations(word) {
        if !parts.contains(&t) {
          parts.push(t);
        }
      }
    }
  }
  if parts.is_empty() {
    return None;
  }

  let mut out = Vec::with_capacity(64);
  for part in parts {
    if out.len() >= MAX_EXTRA {
      break;
    }
    out.extend_from_slice(&SEP);
    for &b in &part {
      // Разметка цвета и скобки имён летописи в поиске не нужны
      if b < 0x20 || b == b'{' || b == b'}' {
        continue;
      }
      out.push(CONFIG.encoding.simplify[b as usize]);
      if out.len() >= MAX_EXTRA {
        break;
      }
    }
  }
  Some(out)
}

/// Все русские переводы строки: как есть, с заглавной и строчными
/// буквами — словари хранят надписи в том виде, в каком их нарисовала игра.
fn translations(key: &[u8]) -> Vec<Vec<u8>> {
  let mut variants: Vec<Vec<u8>> = vec![key.to_vec()];
  let mut capital = key.to_vec();
  capital[0] = capital[0].to_ascii_uppercase();
  let lower = key.to_ascii_lowercase();
  for v in [capital, lower] {
    if !variants.contains(&v) {
      variants.push(v);
    }
  }

  let mut out: Vec<Vec<u8>> = Vec::new();
  for v in variants {
    for value in dictionary::search_values(&v) {
      let text = value.strip_suffix(&[0]).unwrap_or(&value).to_vec();
      // Только настоящий перевод: в нём есть русские буквы
      if text.iter().any(|&b| b >= 0x80) && !out.contains(&text) {
        out.push(text);
      }
    }
  }
  out
}

