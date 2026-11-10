# A typical single-container production setup for the Laravel original:
# Apache with mod_php, OPcache on, no dev dependencies, cached config/routes/views.
FROM composer:2 AS vendor
WORKDIR /app
COPY composer.json composer.lock ./
RUN composer install --no-dev --no-interaction --no-scripts --prefer-dist --optimize-autoloader --ignore-platform-reqs

FROM php:8.4-apache
RUN docker-php-ext-install opcache \
 && a2enmod rewrite \
 && sed -ri 's!/var/www/html!/var/www/html/public!g' /etc/apache2/sites-available/*.conf \
 && mv "$PHP_INI_DIR/php.ini-production" "$PHP_INI_DIR/php.ini" \
 && printf 'opcache.enable=1\nopcache.validate_timestamps=0\nopcache.memory_consumption=128\n' > "$PHP_INI_DIR/conf.d/opcache-prod.ini"
WORKDIR /var/www/html
COPY . .
COPY --from=vendor /app/vendor vendor
ARG SESSION_DRIVER=database
ENV APP_ENV=production APP_DEBUG=false LOG_CHANNEL=stderr SESSION_DRIVER=${SESSION_DRIVER}
RUN php artisan package:discover --ansi \
 && php artisan config:cache && php artisan route:cache && php artisan view:cache \
 && chown -R www-data:www-data storage bootstrap/cache database
EXPOSE 80
