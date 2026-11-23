use std::{env, fs, sync::LazyLock, time::Duration};

use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, MultiPart},
    transport::smtp::authentication::Credentials,
};
use sqlx::{PgPool, Postgres, Transaction};
use tokio::sync::Semaphore;

use super::digest;

struct Mailer {
    from: Mailbox,
    smtp: AsyncSmtpTransport<Tokio1Executor>,
}

static MAILER: LazyLock<Option<Mailer>> = LazyLock::new(|| {
    let path = env::var_os("ADDRESSWISE_SMTP_PASSWORD_FILE")?;
    let password = fs::read_to_string(path).ok()?;
    let password = password.trim_end_matches(['\r', '\n']);
    if password.is_empty() {
        return None;
    }
    let address = "peter@addresswise.eu";
    let from = Mailbox::new(Some("Addresswise".into()), address.parse().ok()?);
    let smtp = AsyncSmtpTransport::<Tokio1Executor>::relay("smtp.seznam.cz")
        .ok()?
        .port(465)
        .credentials(Credentials::new(address.into(), password.into()))
        .timeout(Some(Duration::from_secs(12)))
        .build();
    Some(Mailer { from, smtp })
});

static MAIL_SLOTS: Semaphore = Semaphore::const_new(4);

#[derive(Clone, Copy)]
pub(super) enum Purpose {
    Confirm,
    Reset,
}

impl Purpose {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::Reset => "reset",
        }
    }

    fn link_action(self) -> &'static str {
        match self {
            Self::Confirm => "confirm-email",
            Self::Reset => "reset-password",
        }
    }

    fn expiry_seconds(self) -> i32 {
        match self {
            Self::Confirm => 24 * 60 * 60,
            Self::Reset => 30 * 60,
        }
    }
}

pub(crate) fn enabled() -> bool {
    MAILER.is_some()
}

pub(super) fn configured() -> bool {
    env::var_os("ADDRESSWISE_SMTP_PASSWORD_FILE").is_some()
}

pub(super) async fn store(
    tx: &mut Transaction<'_, Postgres>,
    owner: i64,
    purpose: Purpose,
    token: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO customer_email_actions(token_hash,customer_id,purpose,expires_at)
         VALUES($1,$2,$3,now()+make_interval(secs=>$4))",
    )
    .bind(digest(token))
    .bind(owner)
    .bind(purpose.name())
    .bind(f64::from(purpose.expiry_seconds()))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(super) async fn revoke(
    tx: &mut Transaction<'_, Postgres>,
    owner: i64,
    purpose: Purpose,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM customer_email_actions WHERE customer_id=$1 AND purpose=$2")
        .bind(owner)
        .bind(purpose.name())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(super) async fn consume(
    tx: &mut Transaction<'_, Postgres>,
    owner: i64,
    purpose: Purpose,
    token_hash: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "DELETE FROM customer_email_actions
                    WHERE token_hash=$1 AND customer_id=$2 AND purpose=$3 AND expires_at>now()",
    )
    .bind(token_hash)
    .bind(owner)
    .bind(purpose.name())
    .execute(&mut **tx)
    .await?
    .rows_affected()
        == 1)
}

pub(super) fn valid_token(raw: &str) -> bool {
    raw.len() == 43
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

pub(super) fn send(
    pool: PgPool,
    recipient: String,
    language: String,
    purpose: Purpose,
    token: String,
) {
    let Some(mailer) = MAILER.as_ref() else {
        return;
    };
    let Ok(to) = recipient.parse() else { return };
    let Some(message) = account_message(mailer.from.clone(), to, &language, purpose, &token) else {
        return;
    };
    tokio::spawn(async move {
        let Ok(_slot) = MAIL_SLOTS.acquire().await else {
            return;
        };
        for delay in [0, 5, 30] {
            if delay > 0 {
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
            match mailer.smtp.send(message.clone()).await {
                Ok(_) => break,
                Err(error) if error.is_permanent() || delay == 30 => {
                    eprintln!("Addresswise account email delivery failed");
                    break;
                }
                Err(_) => {}
            }
        }
        // Sweep expired action hashes without logging recipient addresses or link tokens.
        let _ = sqlx::query("DELETE FROM customer_email_actions WHERE expires_at<=now()")
            .execute(&pool)
            .await;
    });
}

struct EmailCopy {
    subject: &'static str,
    heading: &'static str,
    instruction: &'static str,
    button: &'static str,
    expiry: &'static str,
    unsolicited: &'static str,
}

fn account_message(
    from: Mailbox,
    to: Mailbox,
    language: &str,
    purpose: Purpose,
    token: &str,
) -> Option<Message> {
    if !valid_token(token) {
        return None;
    }
    let language = match language {
        "cs" => "cs",
        "sk" => "sk",
        _ => "en",
    };
    let link = format!(
        "https://addresswise.eu/account?lang={language}#{}={token}",
        purpose.link_action()
    );
    let copy = email_copy(language, purpose);
    let plain = format!(
        "{heading}\n\n{instruction}\n\n{button}:\n{link}\n\n{expiry}\n{unsolicited}\n\nAddresswise\npeter@addresswise.eu\n",
        heading = copy.heading,
        instruction = copy.instruction,
        button = copy.button,
        expiry = copy.expiry,
        unsolicited = copy.unsolicited,
    );
    // The link contains only a fixed host, a supported locale and a URL-safe token.
    let html = format!(
        include_str!("../../static/email/account-action.html"),
        language = language,
        heading = copy.heading,
        instruction = copy.instruction,
        button = copy.button,
        expiry = copy.expiry,
        unsolicited = copy.unsolicited,
        link = link,
    );
    Message::builder()
        .from(from)
        .to(to)
        .subject(copy.subject)
        .multipart(MultiPart::alternative_plain_html(plain, html))
        .ok()
}

fn email_copy(language: &str, purpose: Purpose) -> EmailCopy {
    match (language, purpose) {
        ("cs", Purpose::Confirm) => EmailCopy {
            subject: "Potvrďte registraci v Addresswise",
            heading: "Potvrďte registraci",
            instruction: "Otevřete odkaz a zadejte heslo z registrace. Poté získáte bezplatný API klíč pro Addresswise a Companywise.",
            button: "Potvrdit registraci",
            expiry: "Odkaz platí 24 hodin.",
            unsolicited: "Pokud jste se neregistrovali, můžete tento e-mail ignorovat.",
        },
        ("sk", Purpose::Confirm) => EmailCopy {
            subject: "Potvrďte registráciu v Addresswise",
            heading: "Potvrďte registráciu",
            instruction: "Otvorte odkaz a zadajte heslo z registrácie. Potom získate bezplatný API kľúč pre Addresswise a Companywise.",
            button: "Potvrdiť registráciu",
            expiry: "Odkaz platí 24 hodín.",
            unsolicited: "Ak ste sa neregistrovali, môžete tento e-mail ignorovať.",
        },
        (_, Purpose::Confirm) => EmailCopy {
            subject: "Confirm your Addresswise registration",
            heading: "Confirm your registration",
            instruction: "Open the link and enter the password you chose at registration to activate your Free API key for Addresswise and Companywise.",
            button: "Confirm registration",
            expiry: "The link expires in 24 hours.",
            unsolicited: "If you didn’t register, you can ignore this email.",
        },
        ("cs", Purpose::Reset) => EmailCopy {
            subject: "Obnovte heslo v Addresswise",
            heading: "Nastavte si nové heslo",
            instruction: "Otevřete odkaz a nastavte nové heslo k účtu Addresswise.",
            button: "Nastavit nové heslo",
            expiry: "Odkaz platí 30 minut a lze ho použít jen jednou.",
            unsolicited: "Pokud jste o změnu nežádali, můžete tento e-mail ignorovat. Vaše heslo zůstane stejné.",
        },
        ("sk", Purpose::Reset) => EmailCopy {
            subject: "Obnovte heslo v Addresswise",
            heading: "Nastavte si nové heslo",
            instruction: "Otvorte odkaz a nastavte nové heslo k účtu Addresswise.",
            button: "Nastaviť nové heslo",
            expiry: "Odkaz platí 30 minút a môžete ho použiť iba raz.",
            unsolicited: "Ak ste o zmenu nežiadali, môžete tento e-mail ignorovať. Vaše heslo zostane rovnaké.",
        },
        (_, Purpose::Reset) => EmailCopy {
            subject: "Reset your Addresswise password",
            heading: "Set a new password",
            instruction: "Open the link to set a new password for your Addresswise account.",
            button: "Set a new password",
            expiry: "The link expires in 30 minutes and works once.",
            unsolicited: "If you didn’t request a reset, you can ignore this email. Your password will stay the same.",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_email_includes_both_formats_and_a_localized_action_link() {
        let from: Mailbox = "peter@addresswise.eu".parse().unwrap();
        let to: Mailbox = "test@example.invalid".parse().unwrap();
        let token = "A".repeat(43);
        for language in ["cs", "sk", "en"] {
            for purpose in [Purpose::Confirm, Purpose::Reset] {
                let message = account_message(from.clone(), to.clone(), language, purpose, &token)
                    .expect("account email");
                let wire = String::from_utf8(message.formatted()).unwrap();
                assert!(wire.contains("multipart/alternative"));
                assert!(wire.contains("text/plain"));
                assert!(wire.contains("text/html"));
                // The HTML part may use quoted-printable line folding.
                let unfolded = wire.replace("=\r\n", "").replace("=3D", "=");
                let link = format!(
                    "https://addresswise.eu/account?lang={language}#{}={token}",
                    purpose.link_action()
                );
                assert_eq!(unfolded.matches(&link).count(), 2);
            }
        }
        assert!(account_message(from, to, "cs", Purpose::Confirm, "invalid").is_none());
    }
}
