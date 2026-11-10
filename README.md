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
- Sessions are held in memory (at most 50,000; anonymous ones are dropped first
  when full), so they are lost on restart.
- Email validation is a simple shape check, not Laravel's full RFC validator.
- "x weeks/months ago" wording is approximated for dates older than a week.

## Running

The stylesheet in `static/build/app.css` is the one the original app's own build
produces (`npm run build`, then `public/build/assets/app-*.css`), copied here
unchanged so both sites render from identical CSS.

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

## Container image and measurements

`docker build -t chirper-rs .` produces a static binary on an empty base image
(about 7 MB), running as a non-root user. The dependency list is embedded with
cargo-auditable so scanners such as Trivy can check it.

`bench/laravel.Dockerfile` builds a production-style image of the Laravel
original (Apache, OPcache, cached config) for comparison; build it from a
checkout of laravel/bootcamp with `-f`. One run on one desktop, each container
limited to 2 CPUs and 512 MB, 50 connections to `/` for 30 seconds:

| | Laravel image | This port |
|---|---|---|
| Pages served | 3,842 | 174,944 |
| Median response | about 100 ms | about 6 ms |
| Peak memory | 170 MB | 37 MB |
| Image size | 539 MB | 7 MB |
| Trivy 0.76.2 findings | 1,431 (all OS packages) | 0 |

Most of the Trivy difference comes from the Debian base of `php:8.4-apache`;
`php:8.4-fpm-alpine` alone had one finding. Laravel Octane was not tested.

## Credits and licence

MIT. Templates, markup and static assets are derived from
[laravel/bootcamp](https://github.com/laravel/bootcamp), whose `composer.json`
declares the MIT licence. The Chirper and Laravel logos in the layout belong to
Laravel and are kept only so the port can be compared with the original.
