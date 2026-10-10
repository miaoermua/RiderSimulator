//! 骑手账号：名字规范化 + 密码哈希。
//!
//! 这一层只回答两个问题——**名字能不能用**、**密码怎么存怎么验**。
//! 不碰数据库（见 [`crate::store`]），不碰世界（登录编排见 `app::do_hello`）。
//!
//! ## 明文密码在系统里活不过一次请求
//!
//! 烽火台建号 / 改密时，明文密码只在**那一次响应里回显**——因为二维码里要放
//! 地址 + 名字 + 密码（见 `docs/技术设计.md` 的「接入」一节）。库里只留
//! Argon2id 的 PHC 字符串 `$argon2id$v=19$m=…$salt$hash`。
//!
//! 所以「忘了密码」只能**重置**，不能找回。这是有意的，不是没做完。
//!
//! ## 为什么名字规则与骑手名共用一套
//!
//! 账号名就是骑手名——改名会把账号也叫成另一个名字，那对不上账。所以
//! 规范化和 [`crate::sim::world::World::sanitize_rider_name`] 用同一条规则
//! （去空白、剔控制字符、截到 12 字）。

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier},
};
use rand::Rng;

/// 密码长度下限。4 位是「够用不烦人」的底线——这是模拟器，不是银行。
pub const MIN_PASSWORD_CHARS: usize = 4;
/// 密码长度上限。防的是「有人粘 10MB 进来」。
pub const MAX_PASSWORD_CHARS: usize = 64;

/// 随机盐的字节数。Argon2 的建议值是 16。
const SALT_BYTES: usize = 16;

/// 规范化账号名（= 骑手名）。`None` = 这名字不能用。
pub fn normalize_name(raw: &str) -> Option<String> {
    crate::sim::world::World::sanitize_rider_name(raw)
}

/// 密码合规性。返回的是**给用户看的中文原因**。
pub fn check_password(password: &str) -> Result<(), String> {
    let n = password.chars().count();
    if n < MIN_PASSWORD_CHARS {
        return Err(format!("密码至少 {MIN_PASSWORD_CHARS} 位"));
    }
    if n > MAX_PASSWORD_CHARS {
        return Err(format!("密码最多 {MAX_PASSWORD_CHARS} 位"));
    }
    Ok(())
}

/// 算密码哈希。失败只可能是底层随机数/编码出问题。
pub fn hash_password(password: &str) -> Result<String, String> {
    // 盐自己生成（16 字节 OS 随机），不走 password-hash 的默认盐生成，
    // 省得为一个盐再开 rand_core 特性。
    Argon2::default()
        .hash_password_with_salt(password.as_bytes(), &random_salt())
        .map(|h| h.to_string())
        .map_err(|e| format!("算密码哈希失败：{e}"))
}

/// 验密码。**哈希串本身坏了也返回 `false`**——不 panic、不区分「哈希坏了」
/// 和「密码不对」，免得给出可探测的差异。
pub fn verify_password(stored_hash: &str, password: &str) -> bool {
    Argon2::default()
        .verify_password(password.as_bytes(), stored_hash)
        .is_ok()
}

/// 随机密码。烽火台「重置密码」用——管理员不需要自己想密码。
///
/// 去掉了 `0/O/1/l/I` 这类抄下来会错的字符：这串东西是要**手抄**的。
pub fn random_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut buf = [0u8; 8];
    rand::rng().fill_bytes(&mut buf);
    buf.iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

fn random_salt() -> [u8; SALT_BYTES] {
    let mut salt = [0u8; SALT_BYTES];
    rand::rng().fill_bytes(&mut salt);
    salt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_cleaned_like_a_rider_name() {
        assert_eq!(normalize_name("  张三 ").as_deref(), Some("张三"));
        assert_eq!(normalize_name("张\u{7}三").as_deref(), Some("张三"));
        assert_eq!(normalize_name("").as_deref(), None);
        assert_eq!(normalize_name("   ").as_deref(), None);
        // 与骑手名共用同一条截断规则
        let long = "一二三四五六七八九十甲乙丙丁";
        assert_eq!(
            normalize_name(long).unwrap().chars().count(),
            crate::sim::world::World::MAX_RIDER_NAME_CHARS
        );
    }

    #[test]
    fn password_length_is_enforced() {
        assert!(check_password("123").is_err());
        assert!(check_password("1234").is_ok());
        assert!(check_password(&"x".repeat(MAX_PASSWORD_CHARS)).is_ok());
        assert!(check_password(&"x".repeat(MAX_PASSWORD_CHARS + 1)).is_err());
    }

    #[test]
    fn hash_is_argon2_phc_and_never_contains_the_password() {
        let hash = hash_password("hunter2333").unwrap();
        assert!(hash.starts_with("$argon2id$"), "应该是 PHC 字符串：{hash}");
        assert!(!hash.contains("hunter2333"));
        // 同一密码两次哈希必须不同（盐不同）
        assert_ne!(hash, hash_password("hunter2333").unwrap());
    }

    #[test]
    fn verify_accepts_the_right_password_only() {
        let hash = hash_password("hunter2333").unwrap();
        assert!(verify_password(&hash, "hunter2333"));
        assert!(!verify_password(&hash, "hunter2334"));
        assert!(!verify_password(&hash, ""));
        assert!(!verify_password("不是哈希串", "hunter2333"));
        assert!(!verify_password("", "hunter2333"));
    }

    #[test]
    fn random_password_is_sane() {
        let a = random_password();
        assert_eq!(a.chars().count(), 8);
        assert!(check_password(&a).is_ok());
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(a, random_password(), "两次随机不该撞上");
    }
}
