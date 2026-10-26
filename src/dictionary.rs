use anyhow::Result;
use std::collections::HashMap;
use std::io::prelude::*;

use crate::ai;
use crate::constants::PATH_DICTIONARY;
use crate::utils;

#[static_init::dynamic]
pub static DICTIONARY: Dictionary = Dictionary::new(PATH_DICTIONARY);

#[allow(dead_code)]
pub struct Dictionary {
  map: HashMap<Vec<u8>, Vec<u8>>,
  path: &'static str,
}

impl Dictionary {
  pub fn new(path: &'static str) -> Self {
    Self {
      map: match Dictionary::load(path) {
        Ok(value) => value,
        Err(_) => {
          log::error!("unable to load dictionary {path}");
          utils::message_box(
            "dfint hook error",
            format!("Unable to load dictionary {path}").as_str(),
            utils::MessageIconType::Warning,
          );
          HashMap::<Vec<u8>, Vec<u8>>::new()
        }
      },
      path,
    }
  }

  pub fn get(&self, key: &[u8]) -> Option<&Vec<u8>> {
    self.map.get(key)
  }

  pub fn size(&self) -> usize {
    self.map.len()
  }

  pub fn _data(&self) -> &HashMap<Vec<u8>, Vec<u8>> {
    &self.map
  }

  pub fn _reload(&mut self) -> Result<()> {
    self.map = Self::load(self.path)?;
    Ok(())
  }

  /// Разбор CSV-словаря. Открыт для модуля ai: второй словарь
  /// читается тем же разбором, чтобы формат файлов был одинаковым.
  #[allow(unused_must_use)]
  pub fn load(path: &str) -> Result<HashMap<Vec<u8>, Vec<u8>>> {
    let mut file = std::fs::File::open(path)?;
    let mut contents: Vec<u8> = Vec::new();
    file.read_to_end(&mut contents);
    let mut map = HashMap::<Vec<u8>, Vec<u8>>::new();
    const QUOTE: &u8 = &b"\""[0];
    for item in regex::bytes::Regex::new(r#"(?-u)"(.+)","(.+)""#)?.captures_iter(&contents) {
      let mut k = item[1].to_vec();
      let mut v = item[2].to_vec();
      v.push(0);
      k.dedup_by(|a, b| a == QUOTE && b == QUOTE);
      v.dedup_by(|a, b| a == QUOTE && b == QUOTE);
      map.insert(k, v);
    }
    Ok(map)
  }
}

/// Служебные слова: строка только из них — кусок фразы, а не надпись.
/// Сюда же — слова, с которых игра начинает предложения связного текста
/// («You can…», «All available…», «Here…»): одно такое слово с заглавной
/// — начало фразы, а не надпись. Заменённое при сборке строки на
/// «Вы» / «Все», оно сбивает раскладку слов, и фраза рвётся.
const FUNCTION_WORDS: &[&[u8]] = &[
  b"this", b"is", b"it", b"there", b"was", b"were", b"a", b"an", b"the", b"he", b"she", b"they", b"his", b"her",
  b"their", b"its", b"of", b"in", b"on", b"at", b"to", b"and", b"or", b"by", b"with", b"from", b"for", b"as",
  b"you", b"your", b"all", b"some", b"any", b"each", b"every", b"these", b"those", b"that", b"we", b"our", b"i",
  b"my", b"here", b"when", b"if", b"so", b"then", b"also", b"but", b"can", b"will", b"may", b"must", b"should",
  b"would", b"could", b"are", b"be", b"has", b"have", b"had", b"do", b"does", b"such", b"which", b"who", b"what",
  b"where", b"once", b"only", b"many", b"most", b"more",
];

/// Надпись интерфейса, а не кусок, из которого игра собирает текст.
///
/// В режиме оригинального текста словарь сообщества при сборке строк
/// подставляется только для надписей: строка начинается с заглавной
/// латинской буквы (или с разметки цвета «[»), пробел на конце — только
/// после двоеточия («Folder: »), и это не одни служебные слова («This
/// is»). Не подставляется для кусков фраз (« was a legendary », «This
/// must be the will of »), для слов языков, из которых игра склеивает
/// имена («night», «void» → «Ночьюпустой»), и для «the», который
/// словарь превращает в пробел. Из таких кусков игра собирает летопись,
/// мысли и описания — их переводит нейросеть с английского целиком.
fn interface_label(key: &[u8]) -> bool {
  let Some(&first) = key.first() else {
    return false;
  };
  if !(first.is_ascii_uppercase() || first == b'[') {
    return false;
  }
  let trimmed = key.trim_ascii_end();
  if trimmed.len() != key.len() && trimmed.last() != Some(&b':') {
    return false;
  }
  let words: Vec<&[u8]> = trimmed.split(|&b| b == b' ').filter(|w| !w.is_empty()).collect();
  // Одно слово с заглавной («Select», «Workshops», «Farming») — так игра
  // собирает по слову окна обучения и летопись, и перевод сообщества при
  // сборке вставлял в английскую фразу русские слова («Выбор Farmplot
  // from the Build menu»). Отдельная надпись из одного слова («Settings»,
  // «Items») переводится так же, только при выводе на экран. Исключение —
  // подпись с двоеточием («Squad:»)
  if words.len() == 1 && trimmed.last() != Some(&b':') {
    return false;
  }
  let only_function_words = words.iter().all(|w| FUNCTION_WORDS.iter().any(|f| w.eq_ignore_ascii_case(f)));
  if only_function_words {
    return false;
  }
  // Предложение из трёх слов и больше — речь или описание («I've been
  // rained on.»), а не надпись: его переводит нейросеть, с учётом пола
  // дварфа и всей фразы. Словарь сообщества дал бы мужской род для всех
  let sentence = matches!(trimmed.last(), Some(b'.' | b'!' | b'?' | b'"'));
  !(sentence && words.len() >= 3)
}

/// Словарь сообщества. В режиме оригинального текста при сборке строк
/// (construction) — только надписи интерфейса; при выводе на экран —
/// любая строка целиком: она уже готова, и перевод ничего не склеит.
fn community(key: &[u8], construction: bool) -> Option<Vec<u8>> {
  if ai::EXPERIMENTS.english {
    if construction && !interface_label(key) {
      return None;
    }
    // Перевод сообщества не помещается на экране — у приложения есть свой,
    // под место (ключ с приставкой 0x18, см. writer.js)
    if !construction && DICTIONARY.get(key).is_some() {
      let mut fit_key = Vec::with_capacity(key.len() + 1);
      fit_key.push(0x18);
      fit_key.extend_from_slice(key);
      if let Some(value) = ai::lookup(&fit_key) {
        return Some(value);
      }
    }
    // При выводе — не отдельное слово строчными: так игра рисует по слову
    // связный текст (окна обучения, летопись), и пословный перевод
    // сообщества даёт кашу «вас назначить где ваш жители». Такие слова
    // ждут перевода всей фразы
    if !construction && !key.contains(&b' ') && key.first().is_some_and(|b| b.is_ascii_lowercase()) {
      return None;
    }
  }
  DICTIONARY.get(key).cloned()
}

/// Перевод этой строки при выводе даст словарь сообщества. Его авторы
/// сами подгоняли надписи под место (пробелы в начале для центровки),
/// поэтому выравнивание (align.rs) такие надписи не сдвигает.
pub fn from_community(key: &[u8]) -> bool {
  DICTIONARY.get(key).is_some()
}

/// Поиск там, где игра копирует и склеивает строки: словарь сообщества,
/// затем базовый (замороженный) нейросетевой словарь. Новые переводы
/// нейросети здесь не применяются: строка может быть не только текстом
/// на экране, но и именем, которое игра запишет в сохранение.
/// В режиме оригинального текста словарь сообщества пропускается — игра
/// собирает строки по-английски, и до экрана доходит настоящий текст.
pub fn lookup(key: &[u8]) -> Option<Vec<u8>> {
  if let Some(value) = community(key, true) {
    return Some(value);
  }
  // Режим оригинального текста: надписи интерфейса из нашего словаря
  // подставляются при сборке строк, как раньше словарь сообщества
  if ai::EXPERIMENTS.english {
    if let Some(value) = ai::lookup_ui(key) {
      return Some(value);
    }
  }
  ai::lookup_core(key)
}

/// Поиск для вывода на экран: словарь сообщества, нейросетевой, затем
/// базовый. На экране перевод ничего в логике игры не меняет, поэтому
/// здесь действуют и новые переводы, и ручные правки.
pub fn lookup_all(key: &[u8]) -> Option<Vec<u8>> {
  if let Some(value) = community(key, false) {
    return Some(value);
  }
  if let Some(value) = ai::lookup(key) {
    return Some(value);
  }
  ai::lookup_core(key)
}

/// Поиск для строк, выводимых на экран. Кроме обычного поиска здесь
/// собираются абзацы из строк, на которые игра режет длинный текст,
/// и подставляется перевод строки с учётом её места в абзаце.
/// Строка без перевода уходит в журнал для переводчика.
pub fn lookup_screen(gps: usize, key: &[u8]) -> Option<Vec<u8>> {
  ai::screen(gps, key, lookup_all)
}
