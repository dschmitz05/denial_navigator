//! Rules for a new password, applied wherever one is set: self-service
//! change, admin registration and admin reset.

pub const MIN_LENGTH: usize = 12;

/// Passwords guessed first. Includes the documented default `admin123`, which
/// is what a seeded account starts with and must be moved off.
const COMMON: &[&str] = &[
    "admin123",
    "administrator",
    "password",
    "password1",
    "password12",
    "password123",
    "password1234",
    "passw0rd",
    "p@ssw0rd",
    "p@ssword123",
    "qwerty",
    "qwerty123",
    "qwertyuiop",
    "letmein",
    "letmein123",
    "welcome",
    "welcome1",
    "welcome123",
    "iloveyou",
    "changeme",
    "changeme123",
    "123456",
    "12345678",
    "123456789",
    "1234567890",
    "123456789012",
    "abc123",
    "111111",
    "000000",
    "dragon",
    "monkey",
    "football",
    "baseball",
    "sunshine",
    "princess",
    "trustno1",
    "superman",
    "master",
    "hospital",
    "healthcare",
    "billing",
    "denial",
];

/// Why `new` may not be used as the password for `username`, if it may not.
pub fn check_new_password(username: &str, new: &str) -> Result<(), String> {
    if new.chars().count() < MIN_LENGTH {
        return Err(format!("Password must be at least {MIN_LENGTH} characters"));
    }
    let lowered = new.to_lowercase();
    let user = username.trim().to_lowercase();
    if user.len() >= 3 && lowered.contains(&user) {
        return Err("Password must not contain the username".into());
    }
    if COMMON.contains(&lowered.as_str()) {
        return Err("That password is too common; choose another".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_new_password;

    #[test]
    fn accepts_a_long_unrelated_password() {
        assert!(check_new_password("jsmith", "correct-horse-staple").is_ok());
    }

    #[test]
    fn rejects_short_passwords() {
        assert!(check_new_password("jsmith", "Sh0rt!").is_err());
    }

    #[test]
    fn rejects_the_username_inside_the_password() {
        assert!(check_new_password("jsmith", "JSmith-2026-spring").is_err());
    }

    #[test]
    fn rejects_common_passwords_in_any_case() {
        assert!(check_new_password("admin", "Password1234").is_err());
        assert!(check_new_password("someone", "123456789012").is_err());
    }
}
