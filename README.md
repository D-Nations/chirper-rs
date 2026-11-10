# chirper-rs

A Rust port of **Chirper**, the demo app from the official
[Laravel Bootcamp](https://github.com/laravel/bootcamp) (Laravel 12, Blade, Tailwind, SQLite).

It is a route-for-route conversion: same pages, same markup, same messages,
same database schema. It exists as a small before/after case study for
migrating a Laravel site to Rust.

This is an unofficial port. It is not affiliated with or endorsed by Laravel.

## Stack

| | Laravel original | This port |
|---|---|---|
| Language / framework | PHP 8.2+, Laravel 12 | Rust, [axum](https://github.com/tokio-rs/axum) |
| Templates | Blade | [Askama](https://github.com/askama-rs/askama) (compiled into the binary) |
| Database | SQLite via Eloquent | SQLite via [sqlx](https://github.com/launchbadge/sqlx), bound parameters only |
| Sessions | Laravel session + CSRF middleware | [tower-sessions](https://github.com/maxcountryman/tower-sessions) + per-session CSRF token |
| Passwords | bcrypt, cost 12 | bcrypt, cost 12, written in PHP's `$2y$` format |

## Routes

| Route | Behaviour |
|---|---|
| `GET /` | Latest 50 chirps |
| `POST /chirps` | Create (signed-in only) |
| `GET /chirps/{id}/edit` | Edit form (owner only) |
| `POST /chirps/{id}` with `_method=PUT` / `DELETE` | Update / delete (owner only) |
| `GET`, `POST /register` | Create account (guests only) |
| `GET`, `POST /login` | Sign in (guests only) |
| `POST /logout` | Sign out |

## What the port adds

- A strict Content-Security-Policy. The one inline `onclick` handler in the
  original became `static/confirm.js`, so no inline script is allowed to run.
- `X-Frame-Options`, `X-Content-Type-Options` and `Referrer-Policy` headers.
- Login takes the same time whether or not the email exists.
- Error detail is logged server-side and never shown to visitors.

## What is not identical

- "Remember me" extends the session to 30 days instead of issuing a separate
  remember-token cookie.
- Sessions are held in memory, so they are lost on restart.
- Email validation is a simple shape check, not Laravel's full RFC validator.
- "x weeks/months ago" wording is approximated for dates older than a week.

## Running

The stylesheet is the one Laravel's own build produces. Build the original
app's assets (`npm run build`), then copy `public/build/assets/app-*.css` to
`static/build/app.css` here. Without it the page loads but is unstyled.

```bash
cargo run -- seed   # optional: 3 demo users and 6 chirps (password: "password")
cargo run           # http://127.0.0.1:8002
```

Settings (environment variables): `DATABASE_PATH` (default `database.sqlite`),
`BIND_ADDR` (default `127.0.0.1:8002`), `STATIC_DIR` (default `static`),
`COOKIE_SECURE=true` when serving over HTTPS.

`DATABASE_PATH` can point at a database created by the Laravel app's own
migrations; both apps can read and write the same file.

```bash
cargo test
```

## Credits and licence

MIT. Templates, markup and static assets are derived from
[laravel/bootcamp](https://github.com/laravel/bootcamp), whose `composer.json`
declares the MIT licence. The Chirper and Laravel logos in the layout belong to
Laravel and are kept only so the port can be compared with the original.
