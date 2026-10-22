#[cfg(target_os = "windows")]
pub const PATH_SDL2: &'static str = "SDL2.dll";
#[cfg(target_os = "linux")]
pub const PATH_SDL2: &'static str = "libSDL2-2.0.so.0";
#[cfg(target_os = "windows")]
pub const PATH_EXE: &'static str = "./Dwarf Fortress.exe";
#[cfg(target_os = "linux")]
pub const PATH_EXE: &'static str = "./dwarfort";

pub const PATH_ENCODING: &'static str = "./dfint-data/encoding.toml";
pub const PATH_CONFIG: &'static str = "./dfint-data/config.toml";
pub const PATH_OFFSETS: &'static str = "./dfint-data/offsets.toml";
pub const PATH_DICTIONARY: &'static str = "./dfint-data/dictionary.csv";

// Нейроперевод: второй словарь и журнал непереведённых строк.
// Обменом с ними занимается внешнее приложение-переводчик.
pub const PATH_AI_DIR: &'static str = "./dfint-data/ai";
pub const PATH_AI_DICTIONARY: &'static str = "./dfint-data/ai/dictionary_ai.csv";
pub const PATH_AI_MISSES: &'static str = "./dfint-data/ai/misses.txt";
pub const PATH_AI_EXPERIMENTS: &'static str = "./dfint-data/ai/experiments.txt";
