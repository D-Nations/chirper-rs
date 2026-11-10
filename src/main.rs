//! Chirper — a Rust port of the Laravel Bootcamp demo app.
//!
//! Route-for-route equivalent of the Laravel original (routes/web.php), using
//! the same SQLite schema so it can run against a database Laravel created.

use std::collections::HashMap;

use askama::Template;
use axum::{
    Router,
    extract::{Form, Path, State},
    http::{HeaderName, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use chrono::{NaiveDateTime, Utc};
use serde::Deserialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use tower_http::{services::ServeDir, set_header::SetResponseHeaderLayer};
use tower_sessions::{
    Expiry, MemoryStore, Session, SessionManagerLayer,
    cookie::{SameSite, time::Duration},
};

const TS_FORMAT: &str = "%Y-%m-%d %H:%M:%S";
const BCRYPT_COST: u32 = 12; // Laravel 12 default (BCRYPT_ROUNDS=12)

// ---------------------------------------------------------------- errors ---

enum AppError {
    /// Not signed in: redirect to the login page (Laravel's `auth` middleware).
    Login,
    /// Already signed in: redirect home (Laravel's `guest` middleware).
    Home,
    Forbidden,
    NotFound,
    /// CSRF token missing or wrong (Laravel answers 419 Page Expired).
    PageExpired,
    MethodNotAllowed,
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, text) = match self {
            AppError::Login => return Redirect::to("/login").into_response(),
            AppError::Home => return Redirect::to("/").into_response(),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "Unauthorized"),
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not Found"),
            AppError::PageExpired => (StatusCode::from_u16(419).unwrap(), "Page Expired"),
            AppError::MethodNotAllowed => (StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed"),
            AppError::Internal(detail) => {
                // Log the detail server-side; never show it to the visitor.
                eprintln!("internal error: {detail}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Server Error")
            }
        };
        (status, text).into_response()
    }
}

macro_rules! internal_from {
    ($($t:ty),*) => {$(
        impl From<$t> for AppError {
            fn from(e: $t) -> Self { AppError::Internal(e.to_string()) }
        }
    )*};
}
internal_from!(
    sqlx::Error,
    askama::Error,
    tower_sessions::session::Error,
    bcrypt::BcryptError,
    tokio::task::JoinError
);

type AppResult<T = Response> = Result<T, AppError>;

// ----------------------------------------------------------- view models ---

struct User {
    id: i64,
    name: String,
}

/// Values every page needs: who is signed in, the CSRF token, the flash toast.
struct Ctx {
    user: Option<User>,
    csrf: String,
    flash: Option<String>,
}

type Errors = HashMap<String, String>;

/// Previously submitted input, re-shown after a validation failure.
#[derive(Default)]
struct Old(HashMap<String, String>);

impl Old {
    fn val(&self, key: &str) -> &str {
        self.0.get(key).map(String::as_str).unwrap_or("")
    }
}

struct ChirpView {
    id: i64,
    message: String,
    user_name: String,
    avatar_key: String,
    ago: String,
    edited: bool,
    owned: bool,
}

struct EditChirp {
    id: i64,
}

#[derive(Template)]
#[template(path = "home.html")]
struct HomeTpl {
    ctx: Ctx,
    chirps: Vec<ChirpView>,
    errors: Errors,
    old: Old,
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTpl {
    ctx: Ctx,
    errors: Errors,
    old: Old,
}

#[derive(Template)]
#[template(path = "register.html")]
struct RegisterTpl {
    ctx: Ctx,
    errors: Errors,
    old: Old,
}

#[derive(Template)]
#[template(path = "edit.html")]
struct EditTpl {
    ctx: Ctx,
    chirp: EditChirp,
    message_value: String,
    errors: Errors,
}

fn render(tpl: impl Template) -> AppResult {
    Ok(Html(tpl.render()?).into_response())
}

// ------------------------------------------------------- session helpers ---

fn random_token() -> String {
    format!("{:032x}{:032x}", rand::random::<u128>(), rand::random::<u128>())
}

async fn csrf_token(session: &Session) -> AppResult<String> {
    if let Some(token) = session.get::<String>("csrf").await? {
        return Ok(token);
    }
    let token = random_token();
    session.insert("csrf", &token).await?;
    Ok(token)
}

/// Compare without leaking, through timing, how many leading bytes matched.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn verify_csrf(session: &Session, submitted: &str) -> AppResult<()> {
    match session.get::<String>("csrf").await? {
        Some(expected) if constant_time_eq(expected.as_bytes(), submitted.as_bytes()) => Ok(()),
        _ => Err(AppError::PageExpired),
    }
}

async fn current_user(session: &Session, db: &SqlitePool) -> AppResult<Option<User>> {
    let Some(id) = session.get::<i64>("user_id").await? else {
        return Ok(None);
    };
    let row = sqlx::query_as::<_, (i64, String)>("select id, name from users where id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|(id, name)| User { id, name }))
}

async fn require_user(session: &Session, db: &SqlitePool) -> AppResult<User> {
    current_user(session, db).await?.ok_or(AppError::Login)
}

async fn require_guest(session: &Session, db: &SqlitePool) -> AppResult<()> {
    match current_user(session, db).await? {
        Some(_) => Err(AppError::Home),
        None => Ok(()),
    }
}

async fn page_ctx(session: &Session, db: &SqlitePool) -> AppResult<Ctx> {
    Ok(Ctx {
        user: current_user(session, db).await?,
        csrf: csrf_token(session).await?,
        flash: session.remove::<String>("flash").await?,
    })
}

/// Validation errors and old input are shown once, then forgotten.
async fn take_form_state(session: &Session) -> AppResult<(Errors, Old)> {
    let errors = session.remove::<Errors>("errors").await?.unwrap_or_default();
    let old = session.remove::<HashMap<String, String>>("old").await?.unwrap_or_default();
    Ok((errors, Old(old)))
}

async fn flash(session: &Session, message: &str) -> AppResult<()> {
    session.insert("flash", message).await?;
    Ok(())
}

async fn back_with_errors(
    session: &Session,
    to: &str,
    errors: Errors,
    old: &[(&str, &str)],
) -> AppResult {
    let old: HashMap<String, String> =
        old.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    session.insert("errors", errors).await?;
    session.insert("old", old).await?;
    Ok(Redirect::to(to).into_response())
}

/// Sign in: new session id (prevents session fixation) and a fresh CSRF token.
async fn sign_in(session: &Session, user_id: i64, remember: bool) -> AppResult<()> {
    session.cycle_id().await?;
    session.insert("user_id", user_id).await?;
    session.insert("csrf", random_token()).await?;
    if remember {
        session.set_expiry(Some(Expiry::OnInactivity(Duration::days(30))));
    }
    Ok(())
}

// ------------------------------------------------------------ validation ---

fn validate_message(raw: &str, custom_messages: bool) -> Result<String, String> {
    let message = raw.trim();
    if message.is_empty() {
        return Err(if custom_messages {
            "Please write something to chirp!".into()
        } else {
            "The message field is required.".into()
        });
    }
    if message.chars().count() > 255 {
        return Err(if custom_messages {
            "Chirps must be 255 characters or less.".into()
        } else {
            "The message field must not be greater than 255 characters.".into()
        });
    }
    Ok(message.to_string())
}

fn looks_like_email(value: &str) -> bool {
    let mut parts = value.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None) => {
            !local.is_empty() && !domain.is_empty() && !value.contains(char::is_whitespace)
        }
        _ => false,
    }
}

// ------------------------------------------------------------- time text ---

fn now() -> String {
    Utc::now().format(TS_FORMAT).to_string()
}

fn parse_ts(value: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(value, TS_FORMAT).unwrap_or_default()
}

/// Equivalent of Carbon's `diffForHumans()` for past dates.
fn diff_for_humans(then: NaiveDateTime, now: NaiveDateTime) -> String {
    let secs = (now - then).num_seconds().max(0);
    let days = secs / 86_400;
    let (count, unit) = match secs {
        s if s < 60 => (s, "second"),
        s if s < 3_600 => (s / 60, "minute"),
        s if s < 86_400 => (s / 3_600, "hour"),
        _ if days < 7 => (days, "day"),
        _ if days < 30 => (days / 7, "week"),
        _ if days < 365 => (days / 30, "month"),
        _ => (days / 365, "year"),
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
}

// -------------------------------------------------------------- handlers ---

async fn home(State(db): State<SqlitePool>, session: Session) -> AppResult {
    let ctx = page_ctx(&session, &db).await?;
    let (errors, old) = take_form_state(&session).await?;
    let viewer_id = ctx.user.as_ref().map(|u| u.id);
    let now = Utc::now().naive_utc();

    let rows = sqlx::query_as::<_, (i64, i64, String, String, String, String, String)>(
        "select c.id, c.user_id, c.message, c.created_at, c.updated_at, u.name, u.email
         from chirps c join users u on u.id = c.user_id
         order by c.created_at desc limit 50",
    )
    .fetch_all(&db)
    .await?;

    let chirps = rows
        .into_iter()
        .map(|(id, user_id, message, created_at, updated_at, user_name, email)| {
            let created = parse_ts(&created_at);
            ChirpView {
                id,
                message,
                user_name,
                avatar_key: urlencoding::encode(&email).into_owned(),
                ago: diff_for_humans(created, now),
                edited: parse_ts(&updated_at) > created + chrono::Duration::seconds(5),
                owned: viewer_id == Some(user_id),
            }
        })
        .collect();

    render(HomeTpl { ctx, chirps, errors, old })
}

#[derive(Deserialize)]
struct ChirpForm {
    #[serde(default, rename = "_token")]
    token: String,
    #[serde(default, rename = "_method")]
    method: String,
    #[serde(default)]
    message: String,
}

async fn store_chirp(
    State(db): State<SqlitePool>,
    session: Session,
    Form(form): Form<ChirpForm>,
) -> AppResult {
    verify_csrf(&session, &form.token).await?;
    let user = require_user(&session, &db).await?;

    let message = match validate_message(&form.message, true) {
        Ok(message) => message,
        Err(error) => {
            let errors = HashMap::from([("message".to_string(), error)]);
            return back_with_errors(&session, "/", errors, &[("message", &form.message)]).await;
        }
    };

    let ts = now();
    sqlx::query("insert into chirps (user_id, message, created_at, updated_at) values (?, ?, ?, ?)")
        .bind(user.id)
        .bind(message)
        .bind(&ts)
        .bind(&ts)
        .execute(&db)
        .await?;

    flash(&session, "Your chirp has been posted!").await?;
    Ok(Redirect::to("/").into_response())
}

/// Load a chirp and enforce that the signed-in user owns it.
async fn owned_chirp(db: &SqlitePool, id: i64, user: &User) -> AppResult<String> {
    let (owner_id, message) =
        sqlx::query_as::<_, (i64, String)>("select user_id, message from chirps where id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?
            .ok_or(AppError::NotFound)?;
    if owner_id != user.id {
        return Err(AppError::Forbidden);
    }
    Ok(message)
}

async fn edit_chirp(
    State(db): State<SqlitePool>,
    session: Session,
    Path(id): Path<i64>,
) -> AppResult {
    let user = require_user(&session, &db).await?;
    let message = owned_chirp(&db, id, &user).await?;
    let ctx = page_ctx(&session, &db).await?;
    let (errors, old) = take_form_state(&session).await?;
    let message_value = match old.0.get("message") {
        Some(previous) => previous.clone(),
        None => message,
    };
    render(EditTpl { ctx, chirp: EditChirp { id }, message_value, errors })
}

/// HTML forms can only POST, so (as in Laravel) a hidden `_method` field
/// selects between update (PUT) and delete (DELETE).
async fn modify_chirp(
    State(db): State<SqlitePool>,
    session: Session,
    Path(id): Path<i64>,
    Form(form): Form<ChirpForm>,
) -> AppResult {
    verify_csrf(&session, &form.token).await?;
    let user = require_user(&session, &db).await?;
    owned_chirp(&db, id, &user).await?;

    match form.method.as_str() {
        "PUT" => {
            let message = match validate_message(&form.message, false) {
                Ok(message) => message,
                Err(error) => {
                    let errors = HashMap::from([("message".to_string(), error)]);
                    let back = format!("/chirps/{id}/edit");
                    return back_with_errors(&session, &back, errors, &[("message", &form.message)])
                        .await;
                }
            };
            sqlx::query("update chirps set message = ?, updated_at = ? where id = ?")
                .bind(message)
                .bind(now())
                .bind(id)
                .execute(&db)
                .await?;
            flash(&session, "Chirp updated!").await?;
        }
        "DELETE" => {
            sqlx::query("delete from chirps where id = ?").bind(id).execute(&db).await?;
            flash(&session, "Chirp deleted!").await?;
        }
        _ => return Err(AppError::MethodNotAllowed),
    }
    Ok(Redirect::to("/").into_response())
}

async fn login_page(State(db): State<SqlitePool>, session: Session) -> AppResult {
    require_guest(&session, &db).await?;
    let ctx = page_ctx(&session, &db).await?;
    let (errors, old) = take_form_state(&session).await?;
    render(LoginTpl { ctx, errors, old })
}

#[derive(Deserialize)]
struct LoginForm {
    #[serde(default, rename = "_token")]
    token: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    remember: Option<String>,
}

/// A bcrypt hash of a random value, checked when the email is unknown so that
/// "no such user" and "wrong password" take the same time to answer.
static DUMMY_HASH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    bcrypt::hash(random_token(), BCRYPT_COST).expect("bcrypt can hash a random token")
});

async fn login(
    State(db): State<SqlitePool>,
    session: Session,
    Form(form): Form<LoginForm>,
) -> AppResult {
    verify_csrf(&session, &form.token).await?;
    require_guest(&session, &db).await?;

    let email = form.email.trim().to_string();
    let mut errors = Errors::new();
    if email.is_empty() {
        errors.insert("email".into(), "The email field is required.".into());
    } else if !looks_like_email(&email) {
        errors.insert("email".into(), "The email field must be a valid email address.".into());
    }
    if form.password.is_empty() {
        errors.insert("password".into(), "The password field is required.".into());
    }
    if !errors.is_empty() {
        return back_with_errors(&session, "/login", errors, &[("email", &email)]).await;
    }

    let row = sqlx::query_as::<_, (i64, String)>("select id, password from users where email = ?")
        .bind(&email)
        .fetch_optional(&db)
        .await?;
    let (user_id, hash) = match row {
        Some((id, hash)) => (Some(id), hash),
        None => (None, DUMMY_HASH.clone()),
    };
    let password = form.password;
    // bcrypt is deliberately slow; keep it off the async worker threads.
    let matches = tokio::task::spawn_blocking(move || bcrypt::verify(password, &hash))
        .await?
        .unwrap_or(false);

    match user_id {
        Some(id) if matches => {
            sign_in(&session, id, form.remember.is_some()).await?;
            flash(&session, "Welcome back!").await?;
            Ok(Redirect::to("/").into_response())
        }
        _ => {
            let errors = HashMap::from([(
                "email".to_string(),
                "The provided credentials do not match our records.".to_string(),
            )]);
            back_with_errors(&session, "/login", errors, &[("email", &email)]).await
        }
    }
}

async fn register_page(State(db): State<SqlitePool>, session: Session) -> AppResult {
    require_guest(&session, &db).await?;
    let ctx = page_ctx(&session, &db).await?;
    let (errors, old) = take_form_state(&session).await?;
    render(RegisterTpl { ctx, errors, old })
}

#[derive(Deserialize)]
struct RegisterForm {
    #[serde(default, rename = "_token")]
    token: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    password_confirmation: String,
}

async fn register(
    State(db): State<SqlitePool>,
    session: Session,
    Form(form): Form<RegisterForm>,
) -> AppResult {
    verify_csrf(&session, &form.token).await?;
    require_guest(&session, &db).await?;

    let name = form.name.trim().to_string();
    let email = form.email.trim().to_string();
    let mut errors = Errors::new();

    if name.is_empty() {
        errors.insert("name".into(), "The name field is required.".into());
    } else if name.chars().count() > 255 {
        errors.insert(
            "name".into(),
            "The name field must not be greater than 255 characters.".into(),
        );
    }

    if email.is_empty() {
        errors.insert("email".into(), "The email field is required.".into());
    } else if !looks_like_email(&email) {
        errors.insert("email".into(), "The email field must be a valid email address.".into());
    } else if email.chars().count() > 255 {
        errors.insert(
            "email".into(),
            "The email field must not be greater than 255 characters.".into(),
        );
    } else {
        let taken = sqlx::query_as::<_, (i64,)>("select id from users where email = ?")
            .bind(&email)
            .fetch_optional(&db)
            .await?
            .is_some();
        if taken {
            errors.insert("email".into(), "The email has already been taken.".into());
        }
    }

    if form.password.is_empty() {
        errors.insert("password".into(), "The password field is required.".into());
    } else if form.password.chars().count() < 8 {
        errors.insert(
            "password".into(),
            "The password field must be at least 8 characters.".into(),
        );
    } else if form.password != form.password_confirmation {
        errors.insert(
            "password".into(),
            "The password field confirmation does not match.".into(),
        );
    }

    if !errors.is_empty() {
        let old = [("name", name.as_str()), ("email", email.as_str())];
        return back_with_errors(&session, "/register", errors, &old).await;
    }

    let user_id = create_user(&db, &name, &email, form.password, &now()).await?;
    sign_in(&session, user_id, false).await?;
    flash(&session, "Welcome to Chirper!").await?;
    Ok(Redirect::to("/").into_response())
}

async fn create_user(
    db: &SqlitePool,
    name: &str,
    email: &str,
    password: String,
    ts: &str,
) -> AppResult<i64> {
    // `$2y$` is the bcrypt variant PHP writes, so either app can verify
    // passwords stored by the other.
    let hash = tokio::task::spawn_blocking(move || {
        bcrypt::hash_with_result(password, BCRYPT_COST)
            .map(|parts| parts.format_for_version(bcrypt::Version::TwoY))
    })
    .await??;
    let result = sqlx::query(
        "insert into users (name, email, password, created_at, updated_at) values (?, ?, ?, ?, ?)",
    )
    .bind(name)
    .bind(email)
    .bind(hash)
    .bind(ts)
    .bind(ts)
    .execute(db)
    .await?;
    Ok(result.last_insert_rowid())
}

#[derive(Deserialize)]
struct TokenForm {
    #[serde(default, rename = "_token")]
    token: String,
}

async fn logout(
    State(db): State<SqlitePool>,
    session: Session,
    Form(form): Form<TokenForm>,
) -> AppResult {
    verify_csrf(&session, &form.token).await?;
    require_user(&session, &db).await?;
    // Throw the whole session away, then start a clean one for the toast.
    session.flush().await?;
    flash(&session, "You have been logged out.").await?;
    Ok(Redirect::to("/").into_response())
}

// ------------------------------------------------------ database / setup ---

/// Same tables Laravel's migrations create, so the two apps can share a file.
const SCHEMA: &[&str] = &[
    "create table if not exists users (
        id integer primary key autoincrement not null,
        name varchar not null,
        email varchar not null,
        email_verified_at datetime,
        password varchar not null,
        remember_token varchar,
        created_at datetime,
        updated_at datetime)",
    "create unique index if not exists users_email_unique on users (email)",
    "create table if not exists chirps (
        id integer primary key autoincrement not null,
        user_id integer not null,
        message varchar not null,
        created_at datetime,
        updated_at datetime,
        foreign key (user_id) references users (id) on delete cascade)",
];

/// Demo content for screenshots: (author index, minutes ago, edited, message).
const DEMO_USERS: &[(&str, &str)] = &[
    ("Ada Okafor", "ada@example.com"),
    ("Mateo Ruiz", "mateo@example.com"),
    ("Priya Nair", "priya@example.com"),
];
const DEMO_CHIRPS: &[(usize, i64, bool, &str)] = &[
    (0, 4, false, "Just shipped the new onboarding flow. Coffee first, then bug reports."),
    (1, 37, true, "Reminder: the meetup moved to Thursday at 6pm. Same room, better snacks."),
    (2, 95, false, "Hot take: a tidy migration history is a love letter to your future self."),
    (0, 260, false, "Anyone have a favourite talk on database indexing? Building a reading list."),
    (1, 1_500, false, "Finally fixed the flaky test. It was a timezone. It is always a timezone."),
    (2, 4_400, false, "Hello, Chirper! First post from the new account."),
];
const DEMO_PASSWORD: &str = "password";

async fn seed(db: &SqlitePool) -> AppResult<()> {
    let (existing,) = sqlx::query_as::<_, (i64,)>("select count(*) from chirps").fetch_one(db).await?;
    if existing > 0 {
        println!("database already has chirps; nothing seeded");
        return Ok(());
    }
    let started = Utc::now();
    let mut ids = Vec::new();
    for (name, email) in DEMO_USERS {
        ids.push(create_user(db, name, email, DEMO_PASSWORD.to_string(), &now()).await?);
    }
    for (author, minutes_ago, edited, message) in DEMO_CHIRPS {
        let created = started - chrono::Duration::minutes(*minutes_ago);
        let updated = if *edited { created + chrono::Duration::minutes(3) } else { created };
        sqlx::query("insert into chirps (user_id, message, created_at, updated_at) values (?, ?, ?, ?)")
            .bind(ids[*author])
            .bind(*message)
            .bind(created.format(TS_FORMAT).to_string())
            .bind(updated.format(TS_FORMAT).to_string())
            .execute(db)
            .await?;
    }
    println!("seeded {} users and {} chirps", DEMO_USERS.len(), DEMO_CHIRPS.len());
    Ok(())
}

fn security_header(name: &'static str, value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::if_not_present(
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    )
}

/// Only our own scripts and the three hosts the original layout already loads
/// styles, fonts and avatars from. No inline script is allowed to run.
const CSP: &str = "default-src 'self'; script-src 'self'; \
    style-src 'self' https://cdn.jsdelivr.net https://fonts.bunny.net; \
    font-src https://fonts.bunny.net https://cdn.jsdelivr.net; \
    img-src 'self' data: https://avatars.laravel.cloud; \
    form-action 'self'; frame-ancestors 'none'; base-uri 'self'; object-src 'none'";

async fn run() -> AppResult<()> {
    let env = |key: &str, default: &str| std::env::var(key).unwrap_or_else(|_| default.to_string());
    let db_path = env("DATABASE_PATH", "database.sqlite");
    let static_dir = env("STATIC_DIR", "static");
    let addr = env("BIND_ADDR", "127.0.0.1:8002");
    let https_only = env("COOKIE_SECURE", "false") == "true";

    let db = SqlitePoolOptions::new()
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await?;
    for statement in SCHEMA {
        sqlx::query(*statement).execute(&db).await?;
    }

    if std::env::args().nth(1).as_deref() == Some("seed") {
        return seed(&db).await;
    }

    let sessions = SessionManagerLayer::new(MemoryStore::default())
        .with_name("chirper_session")
        .with_secure(https_only)
        .with_http_only(true)
        .with_same_site(SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(Duration::minutes(120)));

    let app = Router::new()
        .route("/", get(home))
        .route("/chirps", post(store_chirp))
        .route("/chirps/{id}/edit", get(edit_chirp))
        .route("/chirps/{id}", post(modify_chirp))
        .route("/register", get(register_page).post(register))
        .route("/login", get(login_page).post(login))
        .route("/logout", post(logout))
        .fallback_service(ServeDir::new(static_dir))
        .layer(sessions)
        .layer(security_header("content-security-policy", CSP))
        .layer(security_header("x-content-type-options", "nosniff"))
        .layer(security_header("x-frame-options", "DENY"))
        .layer(security_header("referrer-policy", "strict-origin-when-cross-origin"))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, private"),
        ))
        .with_state(db);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| AppError::Internal(format!("cannot bind {addr}: {e}")))?;
    println!("Chirper (Rust) listening on http://{addr}");
    axum::serve(listener, app)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))
}

#[tokio::main]
async fn main() {
    if let Err(AppError::Internal(detail)) = run().await {
        eprintln!("fatal: {detail}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs_ago: i64) -> String {
        let now = NaiveDateTime::default() + chrono::Duration::days(1000);
        diff_for_humans(now - chrono::Duration::seconds(secs_ago), now)
    }

    #[test]
    fn humanised_times_match_carbon_wording() {
        assert_eq!(at(1), "1 second ago");
        assert_eq!(at(59), "59 seconds ago");
        assert_eq!(at(60), "1 minute ago");
        assert_eq!(at(3 * 3600), "3 hours ago");
        assert_eq!(at(86_400), "1 day ago");
        assert_eq!(at(8 * 86_400), "1 week ago");
        assert_eq!(at(70 * 86_400), "2 months ago");
        assert_eq!(at(800 * 86_400), "2 years ago");
    }

    #[test]
    fn message_validation_mirrors_laravel_rules() {
        assert_eq!(validate_message("  hi  ", true).unwrap(), "hi");
        assert_eq!(validate_message("   ", true).unwrap_err(), "Please write something to chirp!");
        assert_eq!(validate_message("", false).unwrap_err(), "The message field is required.");
        assert!(validate_message(&"é".repeat(255), true).is_ok());
        assert!(validate_message(&"é".repeat(256), true).is_err());
    }

    #[test]
    fn email_shape_check() {
        assert!(looks_like_email("ada@example.com"));
        assert!(!looks_like_email("ada"));
        assert!(!looks_like_email("a@b@c"));
        assert!(!looks_like_email("a b@c.com"));
    }

    #[test]
    fn csrf_comparison() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
