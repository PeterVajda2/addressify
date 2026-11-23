use std::{env, fs, sync::LazyLock, time::Duration};

use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor, message::Mailbox,
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
    let link = format!(
        "https://addresswise.eu/account?lang={language}#{}={token}",
        purpose.link_action()
    );
    let (subject, instruction, expiry) = match (language.as_str(), purpose) {
        ("cs", Purpose::Confirm) => (
            "Potvrďte registraci v Addresswise",
            "Registraci dokončíte otevřením odkazu a zadáním hesla z registrace.",
            "Odkaz platí 24 hodin. Pokud jste se neregistrovali, zprávu můžete ignorovat.",
        ),
        ("sk", Purpose::Confirm) => (
            "Potvrďte registráciu v Addresswise",
            "Registráciu dokončíte otvorením odkazu a zadaním hesla z registrácie.",
            "Odkaz platí 24 hodín. Ak ste sa neregistrovali, správu môžete ignorovať.",
        ),
        (_, Purpose::Confirm) => (
            "Confirm your Addresswise registration",
            "Open the link and enter the password you chose to complete registration.",
            "This link expires in 24 hours. If you did not register, you can ignore this email.",
        ),
        ("cs", Purpose::Reset) => (
            "Obnovte heslo v Addresswise",
            "Otevřete odkaz a nastavte si nové heslo.",
            "Odkaz platí 30 minut. Pokud jste o změnu nežádali, zprávu můžete ignorovat.",
        ),
        ("sk", Purpose::Reset) => (
            "Obnovte heslo v Addresswise",
            "Otvorte odkaz a nastavte si nové heslo.",
            "Odkaz platí 30 minút. Ak ste o zmenu nežiadali, správu môžete ignorovať.",
        ),
        (_, Purpose::Reset) => (
            "Reset your Addresswise password",
            "Open the link to set a new password.",
            "This link expires in 30 minutes. If you did not request it, you can ignore this email.",
        ),
    };
    let Ok(to) = recipient.parse() else { return };
    let Ok(message) = Message::builder()
        .from(mailer.from.clone())
        .to(to)
        .subject(subject)
        .body(format!(
            "{instruction}\n\n{link}\n\n{expiry}\n\nAddresswise\n"
        ))
    else {
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
