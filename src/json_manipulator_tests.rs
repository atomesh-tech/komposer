//! Composer 2.10.3's `JsonManipulatorTest`, ported.

use super::JsonManipulator;
use serde_json::json;

/// Runs every data-provider case and reports all failures at once.
fn check_cases<T>(cases: &[T], run: impl Fn(&T) -> Result<(), String>) {
    let failures: Vec<String> = cases.iter().enumerate().filter_map(|(i, c)| run(c).err().map(|e| format!("case #{i}: {e}"))).collect();
    assert!(failures.is_empty(), "{} of {} cases failed:\n{}", failures.len(), cases.len(), failures.join("\n\n"));
}

fn expect(m: &JsonManipulator, expected: &str) -> Result<(), String> {
    let got = m.contents();
    if got == expected { Ok(()) } else { Err(format!("got:\n{got}\nexpected:\n{expected}")) }
}

const LINKS: &[(&str, &str, &str, &str, &str)] = &[
    (
        "{}",
        "require",
        "vendor/baz",
        "qux",
        r#"{
    "require": {
        "vendor/baz": "qux"
    }
}
"#,
    ),
    (
        r#"{
    "foo": "bar"
}"#,
        "require",
        "vendor/baz",
        "qux",
        r#"{
    "foo": "bar",
    "require": {
        "vendor/baz": "qux"
    }
}
"#,
    ),
    (
        r#"{
    "require": {
    }
}"#,
        "require",
        "vendor/baz",
        "qux",
        r#"{
    "require": {
        "vendor/baz": "qux"
    }
}
"#,
    ),
    (
        r#"{
    "empty": "",
    "require": {
        "foo": "bar"
    }
}"#,
        "require",
        "vendor/baz",
        "qux",
        r#"{
    "empty": "",
    "require": {
        "foo": "bar",
        "vendor/baz": "qux"
    }
}
"#,
    ),
    (
        r#"{
    "require":
    {
        "foo": "bar",
        "vendor/baz": "baz"
    }
}"#,
        "require",
        "vendor/baz",
        "qux",
        r#"{
    "require":
    {
        "foo": "bar",
        "vendor/baz": "qux"
    }
}
"#,
    ),
    (
        r#"{
    "require":
    {
        "foo": "bar",
        "vendor/baz": "baz"
    }
}"#,
        "require",
        "vEnDoR/bAz",
        "qux",
        r#"{
    "require":
    {
        "foo": "bar",
        "vendor/baz": "qux"
    }
}
"#,
    ),
    (
        r#"{
    "require":
    {
        "foo": "bar",
        "vendor\/baz": "baz"
    }
}"#,
        "require",
        "vendor/baz",
        "qux",
        r#"{
    "require":
    {
        "foo": "bar",
        "vendor/baz": "qux"
    }
}
"#,
    ),
    (
        r#"{
    "require":
    {
        "foo": "bar",
        "vendor\/baz": "baz"
    }
}"#,
        "require",
        "vEnDoR/bAz",
        "qux",
        r#"{
    "require":
    {
        "foo": "bar",
        "vendor/baz": "qux"
    }
}
"#,
    ),
    (
        r#"{
    "require": {
        "foo": "bar"
    },
    "repositories": [{
        "type": "package",
        "package": {
            "require": {
                "foo": "bar"
            }
        }
    }]
}"#,
        "require",
        "foo",
        "qux",
        r#"{
    "require": {
        "foo": "qux"
    },
    "repositories": [{
        "type": "package",
        "package": {
            "require": {
                "foo": "bar"
            }
        }
    }]
}
"#,
    ),
    (
        r#"{
    "repositories": [{
        "type": "package",
        "package": {
            "require": {
                "foo": "bar"
            }
        }
    }]
}"#,
        "require",
        "foo",
        "qux",
        r#"{
    "repositories": [{
        "type": "package",
        "package": {
            "require": {
                "foo": "bar"
            }
        }
    }],
    "require": {
        "foo": "qux"
    }
}
"#,
    ),
    (
        r#"{
    "require": {
        "php": "5.*"
    }
}"#,
        "require-dev",
        "foo",
        "qux",
        r#"{
    "require": {
        "php": "5.*"
    },
    "require-dev": {
        "foo": "qux"
    }
}
"#,
    ),
    (
        r#"{
    "require": {
        "php": "5.*"
    },
    "require-dev": {
        "foo": "bar"
    }
}"#,
        "require-dev",
        "foo",
        "qux",
        r#"{
    "require": {
        "php": "5.*"
    },
    "require-dev": {
        "foo": "qux"
    }
}
"#,
    ),
    (
        r#"{
    "repositories": [{
        "type": "package",
        "package": {
            "bar": "ba[z",
            "dist": {
                "url": "http...",
                "type": "zip"
            },
            "autoload": {
                "classmap": [ "foo/bar" ]
            }
        }
    }],
    "require": {
        "php": "5.*"
    },
    "require-dev": {
        "foo": "bar"
    }
}"#,
        "require-dev",
        "foo",
        "qux",
        r#"{
    "repositories": [{
        "type": "package",
        "package": {
            "bar": "ba[z",
            "dist": {
                "url": "http...",
                "type": "zip"
            },
            "autoload": {
                "classmap": [ "foo/bar" ]
            }
        }
    }],
    "require": {
        "php": "5.*"
    },
    "require-dev": {
        "foo": "qux"
    }
}
"#,
    ),
    (
        r#"{
    "config": {
        "cache-files-ttl": 0,
        "discard-changes": true
    },
    "minimum-stability": "stable",
    "prefer-stable": false,
    "provide": {
        "heroku-sys/cedar": "14.2016.03.22"
    },
    "repositories": [
        {
            "packagist.org": false
        },
        {
            "type": "package",
            "package": [
                {
                    "type": "metapackage",
                    "name": "anthonymartin/geo-location",
                    "version": "v1.0.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "aws/aws-sdk-php",
                    "version": "3.9.4",
                    "require": {
                        "heroku-sys/php": ">=5.5"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "cloudinary/cloudinary_php",
                    "version": "dev-master",
                    "require": {
                        "heroku-sys/ext-curl": "*",
                        "heroku-sys/ext-json": "*",
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "doctrine/annotations",
                    "version": "v1.2.7",
                    "require": {
                        "heroku-sys/php": ">=5.3.2"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "doctrine/cache",
                    "version": "v1.6.0",
                    "require": {
                        "heroku-sys/php": "~5.5|~7.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "doctrine/collections",
                    "version": "v1.3.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.2"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "doctrine/common",
                    "version": "v2.6.1",
                    "require": {
                        "heroku-sys/php": "~5.5|~7.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "doctrine/inflector",
                    "version": "v1.1.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.2"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "doctrine/lexer",
                    "version": "v1.0.1",
                    "require": {
                        "heroku-sys/php": ">=5.3.2"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "geoip/geoip",
                    "version": "v1.16",
                    "require": [],
                    "replace": [],
                    "provide": [],
                    "conflict": {
                        "heroku-sys/ext-geoip": "*"
                    }
                },
                {
                    "type": "metapackage",
                    "name": "giggsey/libphonenumber-for-php",
                    "version": "7.2.5",
                    "require": {
                        "heroku-sys/ext-mbstring": "*"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "guzzlehttp/guzzle",
                    "version": "5.3.0",
                    "require": {
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "guzzlehttp/promises",
                    "version": "1.0.3",
                    "require": {
                        "heroku-sys/php": ">=5.5.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "guzzlehttp/psr7",
                    "version": "1.2.3",
                    "require": {
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "guzzlehttp/ringphp",
                    "version": "1.1.0",
                    "require": {
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "guzzlehttp/streams",
                    "version": "3.0.0",
                    "require": {
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "hipchat/hipchat-php",
                    "version": "v1.4",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "kriswallsmith/buzz",
                    "version": "v0.15",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "league/csv",
                    "version": "8.0.0",
                    "require": {
                        "heroku-sys/ext-mbstring": "*",
                        "heroku-sys/php": ">=5.5.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "league/fractal",
                    "version": "0.13.0",
                    "require": {
                        "heroku-sys/php": ">=5.4"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "mashape/unirest-php",
                    "version": "1.2.1",
                    "require": {
                        "heroku-sys/ext-curl": "*",
                        "heroku-sys/ext-json": "*",
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "mtdowling/jmespath.php",
                    "version": "2.3.0",
                    "require": {
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "palex/phpstructureddata",
                    "version": "v2.0.1",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "psr/http-message",
                    "version": "1.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "react/promise",
                    "version": "v2.2.1",
                    "require": {
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "rollbar/rollbar",
                    "version": "v0.15.0",
                    "require": {
                        "heroku-sys/ext-curl": "*"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "ronanguilloux/isocodes",
                    "version": "1.2.0",
                    "require": {
                        "heroku-sys/ext-bcmath": "*",
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "sendgrid/sendgrid",
                    "version": "2.1.1",
                    "require": {
                        "heroku-sys/php": ">=5.3"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "sendgrid/smtpapi",
                    "version": "0.0.1",
                    "require": {
                        "heroku-sys/php": ">=5.3"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "symfony/css-selector",
                    "version": "v2.8.2",
                    "require": {
                        "heroku-sys/php": ">=5.3.9"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "symfony/http-foundation",
                    "version": "v2.8.2",
                    "require": {
                        "heroku-sys/php": ">=5.3.9"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "symfony/polyfill-php54",
                    "version": "v1.1.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.3"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "symfony/polyfill-php55",
                    "version": "v1.1.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.3"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "thepixeldeveloper/sitemap",
                    "version": "3.0.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "tijsverkoyen/css-to-inline-styles",
                    "version": "1.5.5",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "yiisoft/yii",
                    "version": "1.1.17",
                    "require": {
                        "heroku-sys/php": ">=5.1.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "composer.json/composer.lock",
                    "version": "dev-597511d6d51b96e4a8afeba2c79982e5",
                    "require": {
                        "heroku-sys/php": "~5.6.0",
                        "heroku-sys/ext-newrelic": "*",
                        "heroku-sys/ext-gd": "*",
                        "heroku-sys/ext-redis": "*"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                }
            ]
        }
    ],
    "require": {
        "composer.json/composer.lock": "dev-597511d6d51b96e4a8afeba2c79982e5",
        "anthonymartin/geo-location": "v1.0.0",
        "aws/aws-sdk-php": "3.9.4",
        "cloudinary/cloudinary_php": "dev-master",
        "doctrine/annotations": "v1.2.7",
        "doctrine/cache": "v1.6.0",
        "doctrine/collections": "v1.3.0",
        "doctrine/common": "v2.6.1",
        "doctrine/inflector": "v1.1.0",
        "doctrine/lexer": "v1.0.1",
        "geoip/geoip": "v1.16",
        "giggsey/libphonenumber-for-php": "7.2.5",
        "guzzlehttp/guzzle": "5.3.0",
        "guzzlehttp/promises": "1.0.3",
        "guzzlehttp/psr7": "1.2.3",
        "guzzlehttp/ringphp": "1.1.0",
        "guzzlehttp/streams": "3.0.0",
        "hipchat/hipchat-php": "v1.4",
        "kriswallsmith/buzz": "v0.15",
        "league/csv": "8.0.0",
        "league/fractal": "0.13.0",
        "mashape/unirest-php": "1.2.1",
        "mtdowling/jmespath.php": "2.3.0",
        "palex/phpstructureddata": "v2.0.1",
        "psr/http-message": "1.0",
        "react/promise": "v2.2.1",
        "rollbar/rollbar": "v0.15.0",
        "ronanguilloux/isocodes": "1.2.0",
        "sendgrid/sendgrid": "2.1.1",
        "sendgrid/smtpapi": "0.0.1",
        "symfony/css-selector": "v2.8.2",
        "symfony/http-foundation": "v2.8.2",
        "symfony/polyfill-php54": "v1.1.0",
        "symfony/polyfill-php55": "v1.1.0",
        "thepixeldeveloper/sitemap": "3.0.0",
        "tijsverkoyen/css-to-inline-styles": "1.5.5",
        "yiisoft/yii": "1.1.17",
        "heroku-sys/apache": "^2.4.10",
        "heroku-sys/nginx": "~1.8.0"
    }
}"#,
        "require",
        "foo",
        "qux",
        r#"{
    "config": {
        "cache-files-ttl": 0,
        "discard-changes": true
    },
    "minimum-stability": "stable",
    "prefer-stable": false,
    "provide": {
        "heroku-sys/cedar": "14.2016.03.22"
    },
    "repositories": [
        {
            "packagist.org": false
        },
        {
            "type": "package",
            "package": [
                {
                    "type": "metapackage",
                    "name": "anthonymartin/geo-location",
                    "version": "v1.0.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "aws/aws-sdk-php",
                    "version": "3.9.4",
                    "require": {
                        "heroku-sys/php": ">=5.5"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "cloudinary/cloudinary_php",
                    "version": "dev-master",
                    "require": {
                        "heroku-sys/ext-curl": "*",
                        "heroku-sys/ext-json": "*",
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "doctrine/annotations",
                    "version": "v1.2.7",
                    "require": {
                        "heroku-sys/php": ">=5.3.2"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "doctrine/cache",
                    "version": "v1.6.0",
                    "require": {
                        "heroku-sys/php": "~5.5|~7.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "doctrine/collections",
                    "version": "v1.3.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.2"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "doctrine/common",
                    "version": "v2.6.1",
                    "require": {
                        "heroku-sys/php": "~5.5|~7.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "doctrine/inflector",
                    "version": "v1.1.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.2"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "doctrine/lexer",
                    "version": "v1.0.1",
                    "require": {
                        "heroku-sys/php": ">=5.3.2"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "geoip/geoip",
                    "version": "v1.16",
                    "require": [],
                    "replace": [],
                    "provide": [],
                    "conflict": {
                        "heroku-sys/ext-geoip": "*"
                    }
                },
                {
                    "type": "metapackage",
                    "name": "giggsey/libphonenumber-for-php",
                    "version": "7.2.5",
                    "require": {
                        "heroku-sys/ext-mbstring": "*"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "guzzlehttp/guzzle",
                    "version": "5.3.0",
                    "require": {
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "guzzlehttp/promises",
                    "version": "1.0.3",
                    "require": {
                        "heroku-sys/php": ">=5.5.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "guzzlehttp/psr7",
                    "version": "1.2.3",
                    "require": {
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "guzzlehttp/ringphp",
                    "version": "1.1.0",
                    "require": {
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "guzzlehttp/streams",
                    "version": "3.0.0",
                    "require": {
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "hipchat/hipchat-php",
                    "version": "v1.4",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "kriswallsmith/buzz",
                    "version": "v0.15",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "league/csv",
                    "version": "8.0.0",
                    "require": {
                        "heroku-sys/ext-mbstring": "*",
                        "heroku-sys/php": ">=5.5.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "league/fractal",
                    "version": "0.13.0",
                    "require": {
                        "heroku-sys/php": ">=5.4"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "mashape/unirest-php",
                    "version": "1.2.1",
                    "require": {
                        "heroku-sys/ext-curl": "*",
                        "heroku-sys/ext-json": "*",
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "mtdowling/jmespath.php",
                    "version": "2.3.0",
                    "require": {
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "palex/phpstructureddata",
                    "version": "v2.0.1",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "psr/http-message",
                    "version": "1.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "react/promise",
                    "version": "v2.2.1",
                    "require": {
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "rollbar/rollbar",
                    "version": "v0.15.0",
                    "require": {
                        "heroku-sys/ext-curl": "*"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "ronanguilloux/isocodes",
                    "version": "1.2.0",
                    "require": {
                        "heroku-sys/ext-bcmath": "*",
                        "heroku-sys/php": ">=5.4.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "sendgrid/sendgrid",
                    "version": "2.1.1",
                    "require": {
                        "heroku-sys/php": ">=5.3"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "sendgrid/smtpapi",
                    "version": "0.0.1",
                    "require": {
                        "heroku-sys/php": ">=5.3"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "symfony/css-selector",
                    "version": "v2.8.2",
                    "require": {
                        "heroku-sys/php": ">=5.3.9"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "symfony/http-foundation",
                    "version": "v2.8.2",
                    "require": {
                        "heroku-sys/php": ">=5.3.9"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "symfony/polyfill-php54",
                    "version": "v1.1.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.3"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "symfony/polyfill-php55",
                    "version": "v1.1.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.3"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "thepixeldeveloper/sitemap",
                    "version": "3.0.0",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "tijsverkoyen/css-to-inline-styles",
                    "version": "1.5.5",
                    "require": {
                        "heroku-sys/php": ">=5.3.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "yiisoft/yii",
                    "version": "1.1.17",
                    "require": {
                        "heroku-sys/php": ">=5.1.0"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                },
                {
                    "type": "metapackage",
                    "name": "composer.json/composer.lock",
                    "version": "dev-597511d6d51b96e4a8afeba2c79982e5",
                    "require": {
                        "heroku-sys/php": "~5.6.0",
                        "heroku-sys/ext-newrelic": "*",
                        "heroku-sys/ext-gd": "*",
                        "heroku-sys/ext-redis": "*"
                    },
                    "replace": [],
                    "provide": [],
                    "conflict": []
                }
            ]
        }
    ],
    "require": {
        "composer.json/composer.lock": "dev-597511d6d51b96e4a8afeba2c79982e5",
        "anthonymartin/geo-location": "v1.0.0",
        "aws/aws-sdk-php": "3.9.4",
        "cloudinary/cloudinary_php": "dev-master",
        "doctrine/annotations": "v1.2.7",
        "doctrine/cache": "v1.6.0",
        "doctrine/collections": "v1.3.0",
        "doctrine/common": "v2.6.1",
        "doctrine/inflector": "v1.1.0",
        "doctrine/lexer": "v1.0.1",
        "geoip/geoip": "v1.16",
        "giggsey/libphonenumber-for-php": "7.2.5",
        "guzzlehttp/guzzle": "5.3.0",
        "guzzlehttp/promises": "1.0.3",
        "guzzlehttp/psr7": "1.2.3",
        "guzzlehttp/ringphp": "1.1.0",
        "guzzlehttp/streams": "3.0.0",
        "hipchat/hipchat-php": "v1.4",
        "kriswallsmith/buzz": "v0.15",
        "league/csv": "8.0.0",
        "league/fractal": "0.13.0",
        "mashape/unirest-php": "1.2.1",
        "mtdowling/jmespath.php": "2.3.0",
        "palex/phpstructureddata": "v2.0.1",
        "psr/http-message": "1.0",
        "react/promise": "v2.2.1",
        "rollbar/rollbar": "v0.15.0",
        "ronanguilloux/isocodes": "1.2.0",
        "sendgrid/sendgrid": "2.1.1",
        "sendgrid/smtpapi": "0.0.1",
        "symfony/css-selector": "v2.8.2",
        "symfony/http-foundation": "v2.8.2",
        "symfony/polyfill-php54": "v1.1.0",
        "symfony/polyfill-php55": "v1.1.0",
        "thepixeldeveloper/sitemap": "3.0.0",
        "tijsverkoyen/css-to-inline-styles": "1.5.5",
        "yiisoft/yii": "1.1.17",
        "heroku-sys/apache": "^2.4.10",
        "heroku-sys/nginx": "~1.8.0",
        "foo": "qux"
    }
}
"#,
    ),
];

#[test]
fn add_link() {
    check_cases(LINKS, |&(json, kind, package, constraint, expected)| {
        let mut m = JsonManipulator::new(json).unwrap();
        if !m.add_link(kind, package, constraint, false) {
            return Err("returned false".into());
        }
        expect(&m, expected)
    });
}

const SORTED_LINKS: &[(&str, &str, &str, &str, bool, &str)] = &[
    (
        r#"{
    "require": {
        "vendor/baz": "qux"
    }
}"#,
        "require",
        "foo",
        "bar",
        true,
        r#"{
    "require": {
        "foo": "bar",
        "vendor/baz": "qux"
    }
}
"#,
    ),
    (
        r#"{
    "require": {
        "vendor/baz": "qux"
    }
}"#,
        "require",
        "foo",
        "bar",
        false,
        r#"{
    "require": {
        "vendor/baz": "qux",
        "foo": "bar"
    }
}
"#,
    ),
    (
        r#"{
    "require": {
        "foo": "baz",
        "ext-10gd": "*",
        "ext-2mcrypt": "*",
        "lib-foo": "*",
        "hhvm": "*",
        "php": ">=5.5"
    }
}"#,
        "require",
        "igorw/retry",
        "*",
        true,
        r#"{
    "require": {
        "php": ">=5.5",
        "hhvm": "*",
        "ext-2mcrypt": "*",
        "ext-10gd": "*",
        "lib-foo": "*",
        "foo": "baz",
        "igorw/retry": "*"
    }
}
"#,
    ),
];

#[test]
fn add_link_and_sort_packages() {
    check_cases(SORTED_LINKS, |&(json, kind, package, constraint, sort, expected)| {
        let mut m = JsonManipulator::new(json).unwrap();
        if !m.add_link(kind, package, constraint, sort) {
            return Err("returned false".into());
        }
        expect(&m, expected)
    });
}

/// (label, json, name, expected return, expected contents)
const REMOVE_SUB_NODE: &[(&str, &str, &str, bool, Option<&str>)] = &[
    (
        "works on simple ones first",
        r#"{
    "repositories": {
        "foo": {
            "foo": "bar",
            "bar": "baz"
        },
        "bar": {
            "foo": "bar",
            "bar": "baz"
        }
    }
}"#,
        "foo",
        true,
        Some(
            r#"{
    "repositories": {
        "bar": {
            "foo": "bar",
            "bar": "baz"
        }
    }
}
"#,
        ),
    ),
    (
        "works on simple ones last",
        r#"{
    "repositories": {
        "foo": {
            "foo": "bar",
            "bar": "baz"
        },
        "bar": {
            "foo": "bar",
            "bar": "baz"
        }
    }
}"#,
        "bar",
        true,
        Some(
            r#"{
    "repositories": {
        "foo": {
            "foo": "bar",
            "bar": "baz"
        }
    }
}
"#,
        ),
    ),
    (
        "works on simple ones unique",
        r#"{
    "repositories": {
        "foo": {
            "foo": "bar",
            "bar": "baz"
        }
    }
}"#,
        "foo",
        true,
        Some(
            r#"{
    "repositories": {
    }
}
"#,
        ),
    ),
    (
        "works on simple ones escaped slash",
        r#"{
    "repositories": {
        "foo\/bar": {
            "bar": "baz"
        }
    }
}"#,
        "foo/bar",
        true,
        Some(
            r#"{
    "repositories": {
    }
}
"#,
        ),
    ),
    (
        "works on simple ones middle",
        r#"{
    "repositories": {
        "foo": {
            "foo": "bar",
            "bar": "baz"
        },
        "bar": {
            "foo": "bar",
            "bar": "baz"
        },
        "baz": {
            "foo": "bar",
            "bar": "baz"
        }
    }
}"#,
        "bar",
        true,
        Some(
            r#"{
    "repositories": {
        "foo": {
            "foo": "bar",
            "bar": "baz"
        },
        "baz": {
            "foo": "bar",
            "bar": "baz"
        }
    }
}
"#,
        ),
    ),
    (
        "works on undefined ones",
        r#"{
    "repositories": {
        "main": {
            "foo": "bar",
            "bar": "baz"
        }
    }
}"#,
        "removenotthere",
        true,
        Some(
            r#"{
    "repositories": {
        "main": {
            "foo": "bar",
            "bar": "baz"
        }
    }
}
"#,
        ),
    ),
    (
        "works on child having unmatched name",
        r#"{
    "repositories": {
        "baz": {
            "foo": "bar",
            "bar": "baz"
        }
    }
}"#,
        "bar",
        true,
        Some(
            r#"{
    "repositories": {
        "baz": {
            "foo": "bar",
            "bar": "baz"
        }
    }
}
"#,
        ),
    ),
    (
        "works on child having duplicate name",
        r#"{
    "repositories": {
        "foo": {
            "baz": "qux"
        },
        "baz": {
            "foo": "bar",
            "bar": "baz"
        }
    }
}"#,
        "baz",
        true,
        Some(
            r#"{
    "repositories": {
        "foo": {
            "baz": "qux"
        }
    }
}
"#,
        ),
    ),
    (
        "works on empty repos",
        r#"{
    "repositories": {
    }
}"#,
        "bar",
        true,
        None,
    ),
    (
        "works on empty repos2",
        r#"{
    "repositories": {}
}"#,
        "bar",
        true,
        None,
    ),
    (
        "works on missing repos",
        r#"{
}"#,
        "bar",
        true,
        None,
    ),
    (
        "works on deep repos",
        r#"{
    "repositories": {
        "foo": {
            "package": { "bar": "baz" }
        }
    }
}"#,
        "foo",
        true,
        Some(
            r#"{
    "repositories": {
    }
}
"#,
        ),
    ),
    (
        "works on deep repos with borked texts",
        r#"{
    "repositories": {
        "foo": {
            "package": { "bar": "ba{z" }
        }
    }
}"#,
        "bar",
        true,
        Some(
            r#"{
    "repositories": {
        "foo": {
            "package": { "bar": "ba{z" }
        }
    }
}
"#,
        ),
    ),
    (
        "works on deep repos with borked texts2",
        r#"{
    "repositories": {
        "foo": {
            "package": { "bar": "ba}z" }
        }
    }
}"#,
        "bar",
        true,
        Some(
            r#"{
    "repositories": {
        "foo": {
            "package": { "bar": "ba}z" }
        }
    }
}
"#,
        ),
    ),
    (
        "fails on deep arrays with borked texts",
        r#"{
    "repositories": [
        {
            "package": { "bar": "ba[z" }
        }
    ]
}"#,
        "bar",
        false,
        None,
    ),
    (
        "fails on deep arrays with borked texts2",
        r#"{
    "repositories": [
        {
            "package": { "bar": "ba]z" }
        }
    ]
}"#,
        "bar",
        false,
        None,
    ),
];

#[test]
fn remove_sub_node() {
    check_cases(REMOVE_SUB_NODE, |&(label, json, name, expected, contents)| {
        let mut m = JsonManipulator::new(json).unwrap();
        if m.remove_sub_node("repositories", name) != expected {
            return Err(format!("{label}: did not return {expected}"));
        }
        contents.map_or(Ok(()), |c| expect(&m, c)).map_err(|e| format!("{label}: {e}"))
    });
}

#[test]
fn remove_sub_node_from_require() {
    let mut m = JsonManipulator::new(
        r#"{
    "repositories": [
        {
            "package": {
                "require": {
                    "this/should-not-end-up-in-root-require": "~2.0"
                },
                "require-dev": {
                    "this/should-not-end-up-in-root-require-dev": "~2.0"
                }
            }
        }
    ],
    "require": {
        "package/a": "*",
        "package/b": "*",
        "package/c": "*"
    },
    "require-dev": {
        "package/d": "*"
    }
}"#,
    )
    .unwrap();
    assert!(m.remove_sub_node("require", "package/c"));
    assert!(m.remove_sub_node("require-dev", "package/d"));
    assert_eq!(
        m.contents(),
        r#"{
    "repositories": [
        {
            "package": {
                "require": {
                    "this/should-not-end-up-in-root-require": "~2.0"
                },
                "require-dev": {
                    "this/should-not-end-up-in-root-require-dev": "~2.0"
                }
            }
        }
    ],
    "require": {
        "package/a": "*",
        "package/b": "*"
    },
    "require-dev": {
    }
}
"#
    );
}

#[test]
fn remove_sub_node_preserves_object_type_when_empty() {
    let mut m = JsonManipulator::new(
        r#"{
    "test": {"0": "foo"}
}"#,
    )
    .unwrap();
    assert!(m.remove_sub_node("test", "0"));
    assert_eq!(
        m.contents(),
        r#"{
    "test": {
    }
}
"#
    );
}

#[test]
fn remove_sub_node_preserves_object_type_when_empty2() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
        "preferred-install": {"foo/*": "source"}
    }
}"#,
    )
    .unwrap();
    assert!(m.remove_config_setting("preferred-install.foo/*"));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "preferred-install": {
        }
    }
}
"#
    );
}

#[test]
fn add_sub_node_in_require() {
    let mut m = JsonManipulator::new(
        r#"{
    "repositories": [
        {
            "package": {
                "require": {
                    "this/should-not-end-up-in-root-require": "~2.0"
                },
                "require-dev": {
                    "this/should-not-end-up-in-root-require-dev": "~2.0"
                }
            }
        }
    ],
    "require": {
        "package/a": "*",
        "package/b": "*"
    },
    "require-dev": {
        "package/d": "*"
    }
}"#,
    )
    .unwrap();
    assert!(m.add_sub_node("require", "package/c", &json!("*"), true));
    assert!(m.add_sub_node("require-dev", "package/e", &json!("*"), true));
    assert_eq!(
        m.contents(),
        r#"{
    "repositories": [
        {
            "package": {
                "require": {
                    "this/should-not-end-up-in-root-require": "~2.0"
                },
                "require-dev": {
                    "this/should-not-end-up-in-root-require-dev": "~2.0"
                }
            }
        }
    ],
    "require": {
        "package/a": "*",
        "package/b": "*",
        "package/c": "*"
    },
    "require-dev": {
        "package/d": "*",
        "package/e": "*"
    }
}
"#
    );
}

#[test]
fn add_extra_with_package() {
    let mut m = JsonManipulator::new(
        r#"{
    "repositories": [
        {
            "type": "package",
            "package": {
                "authors": [],
                "extra": {
                    "package-xml": "package.xml"
                }
            }
        }
    ],
    "extra": {
        "auto-append-gitignore": true
    }
}"#,
    )
    .unwrap();
    assert!(m.add_property("extra.foo-bar", &json!(true)));
    assert_eq!(
        m.contents(),
        r#"{
    "repositories": [
        {
            "type": "package",
            "package": {
                "authors": [],
                "extra": {
                    "package-xml": "package.xml"
                }
            }
        }
    ],
    "extra": {
        "auto-append-gitignore": true,
        "foo-bar": true
    }
}
"#
    );
}

#[test]
fn add_config_with_package() {
    let mut m = JsonManipulator::new(
        r#"{
    "repositories": [
        {
            "type": "package",
            "package": {
                "authors": [],
                "extra": {
                    "package-xml": "package.xml"
                }
            }
        }
    ],
    "config": {
        "platform": {
            "php": "5.3.9"
        }
    }
}"#,
    )
    .unwrap();
    assert!(m.add_config_setting("preferred-install.my-organization/stable-package", &json!("dist")));
    assert_eq!(
        m.contents(),
        r#"{
    "repositories": [
        {
            "type": "package",
            "package": {
                "authors": [],
                "extra": {
                    "package-xml": "package.xml"
                }
            }
        }
    ],
    "config": {
        "platform": {
            "php": "5.3.9"
        },
        "preferred-install": {
            "my-organization/stable-package": "dist"
        }
    }
}
"#
    );
}

#[test]
fn add_suggest_with_package() {
    let mut m = JsonManipulator::new(
        r#"{
    "repositories": [
        {
            "type": "package",
            "package": {
                "authors": [],
                "extra": {
                    "package-xml": "package.xml"
                }
            }
        }
    ],
    "suggest": {
        "package": "Description"
    }
}"#,
    )
    .unwrap();
    assert!(m.add_property("suggest.new-package", &json!("new-description")));
    assert_eq!(
        m.contents(),
        r#"{
    "repositories": [
        {
            "type": "package",
            "package": {
                "authors": [],
                "extra": {
                    "package-xml": "package.xml"
                }
            }
        }
    ],
    "suggest": {
        "package": "Description",
        "new-package": "new-description"
    }
}
"#
    );
}

#[test]
fn add_config_setting_escapes() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
    }
}"#,
    )
    .unwrap();
    assert!(m.add_config_setting("test", &json!(r#"a\b"#)));
    assert!(m.add_config_setting("test2", &json!("a\nb\u{c}a")));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "test": "a\\b",
        "test2": "a\nb\fa"
    }
}
"#
    );
}

#[test]
fn add_config_setting_works_from_scratch() {
    let mut m = JsonManipulator::new(
        r#"{
}"#,
    )
    .unwrap();
    assert!(m.add_config_setting("foo.bar", &json!("baz")));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "foo": {
            "bar": "baz"
        }
    }
}
"#
    );
}

#[test]
fn add_config_setting_can_add() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
        "foo": "bar"
    }
}"#,
    )
    .unwrap();
    assert!(m.add_config_setting("bar", &json!("baz")));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "foo": "bar",
        "bar": "baz"
    }
}
"#
    );
}

#[test]
fn add_config_setting_can_overwrite() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
        "foo": "bar",
        "bar": "baz"
    }
}"#,
    )
    .unwrap();
    assert!(m.add_config_setting("foo", &json!("zomg")));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "foo": "zomg",
        "bar": "baz"
    }
}
"#
    );
}

#[test]
fn add_config_setting_can_overwrite_numbers() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
        "foo": 500
    }
}"#,
    )
    .unwrap();
    assert!(m.add_config_setting("foo", &json!(50)));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "foo": 50
    }
}
"#
    );
}

#[test]
fn add_config_setting_can_overwrite_arrays() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
        "github-oauth": {
            "github.com": "foo"
        },
        "github-protocols": ["https"]
    }
}"#,
    )
    .unwrap();
    assert!(m.add_config_setting("github-protocols", &json!(["https", "http"])));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "github-oauth": {
            "github.com": "foo"
        },
        "github-protocols": ["https", "http"]
    }
}
"#
    );
    assert!(m.add_config_setting("github-oauth", &json!({"github.com": "bar", "alt.example.org": "baz"})));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "github-oauth": {
            "github.com": "bar",
            "alt.example.org": "baz"
        },
        "github-protocols": ["https", "http"]
    }
}
"#
    );
}

#[test]
fn add_config_setting_can_add_sub_key_in_empty_config() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
    }
}"#,
    )
    .unwrap();
    assert!(m.add_config_setting("github-oauth.bar", &json!("baz")));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "github-oauth": {
            "bar": "baz"
        }
    }
}
"#
    );
}

#[test]
fn add_config_setting_can_add_sub_key_in_empty_val() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
        "github-oauth": {},
        "github-oauth2": {
        }
    }
}"#,
    )
    .unwrap();
    assert!(m.add_config_setting("github-oauth.bar", &json!("baz")));
    assert!(m.add_config_setting("github-oauth2.a.bar", &json!("baz2")));
    assert!(m.add_config_setting("github-oauth3.b", &json!("c")));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "github-oauth": {
            "bar": "baz"
        },
        "github-oauth2": {
            "a.bar": "baz2"
        },
        "github-oauth3": {
            "b": "c"
        }
    }
}
"#
    );
}

#[test]
fn add_config_setting_can_add_sub_key_in_hash() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
        "github-oauth": {
            "github.com": "foo"
        }
    }
}"#,
    )
    .unwrap();
    assert!(m.add_config_setting("github-oauth.bar", &json!("baz")));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "github-oauth": {
            "github.com": "foo",
            "bar": "baz"
        }
    }
}
"#
    );
}

#[test]
fn add_root_setting_does_not_break_dots() {
    let mut m = JsonManipulator::new(
        r#"{
    "github-oauth": {
        "github.com": "foo"
    }
}"#,
    )
    .unwrap();
    assert!(m.add_sub_node("github-oauth", "bar", &json!("baz"), true));
    assert_eq!(
        m.contents(),
        r#"{
    "github-oauth": {
        "github.com": "foo",
        "bar": "baz"
    }
}
"#
    );
}

#[test]
fn remove_config_setting_can_remove_sub_key_in_hash() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
        "github-oauth": {
            "github.com": "foo",
            "bar": "baz"
        }
    }
}"#,
    )
    .unwrap();
    assert!(m.remove_config_setting("github-oauth.bar"));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "github-oauth": {
            "github.com": "foo"
        }
    }
}
"#
    );
}

#[test]
fn remove_config_setting_can_remove_sub_key_in_hash_with_siblings() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
        "foo": "bar",
        "github-oauth": {
            "github.com": "foo",
            "bar": "baz"
        }
    }
}"#,
    )
    .unwrap();
    assert!(m.remove_config_setting("github-oauth.bar"));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "foo": "bar",
        "github-oauth": {
            "github.com": "foo"
        }
    }
}
"#
    );
}

#[test]
fn add_main_key() {
    let mut m = JsonManipulator::new(
        r#"{
    "foo": "bar"
}"#,
    )
    .unwrap();
    assert!(m.add_main_key("bar", &json!("baz")));
    assert_eq!(
        m.contents(),
        r#"{
    "foo": "bar",
    "bar": "baz"
}
"#
    );
}

#[test]
fn add_main_key_with_content_having_dollar_sign_followed_by_digit() {
    let mut m = JsonManipulator::new(
        r#"{
    "foo": "bar"
}"#,
    )
    .unwrap();
    assert!(m.add_main_key("bar", &json!("$1baz")));
    assert_eq!(
        m.contents(),
        r#"{
    "foo": "bar",
    "bar": "$1baz"
}
"#
    );
}

#[test]
fn add_main_key_with_content_having_dollar_sign_followed_by_digit2() {
    let mut m = JsonManipulator::new("{}").unwrap();
    assert!(m.add_main_key("foo", &json!("$1bar")));
    assert_eq!(
        m.contents(),
        r#"{
    "foo": "$1bar"
}
"#
    );
}

#[test]
fn update_main_key() {
    let mut m = JsonManipulator::new(
        r#"{
    "foo": "bar"
}"#,
    )
    .unwrap();
    assert!(m.add_main_key("foo", &json!("baz")));
    assert_eq!(
        m.contents(),
        r#"{
    "foo": "baz"
}
"#
    );
}

#[test]
fn update_main_key2() {
    let mut m = JsonManipulator::new(
        r#"{
    "a": {
        "foo": "bar",
        "baz": "qux"
    },
    "foo": "bar",
    "baz": "bar"
}"#,
    )
    .unwrap();
    assert!(m.add_main_key("foo", &json!("baz")));
    assert!(m.add_main_key("baz", &json!("quux")));
    assert_eq!(
        m.contents(),
        r#"{
    "a": {
        "foo": "bar",
        "baz": "qux"
    },
    "foo": "baz",
    "baz": "quux"
}
"#
    );
}

#[test]
fn update_main_key3() {
    let mut m = JsonManipulator::new(
        r#"{
    "require": {
        "php": "5.*"
    },
    "require-dev": {
        "foo": "bar"
    }
}"#,
    )
    .unwrap();
    assert!(m.add_main_key("require-dev", &json!({"foo": "qux"})));
    assert_eq!(
        m.contents(),
        r#"{
    "require": {
        "php": "5.*"
    },
    "require-dev": {
        "foo": "qux"
    }
}
"#
    );
}

#[test]
fn update_main_key_with_content_having_dollar_sign_followed_by_digit() {
    let mut m = JsonManipulator::new(
        r#"{
    "foo": "bar"
}"#,
    )
    .unwrap();
    assert!(m.add_main_key("foo", &json!("$1bar")));
    assert_eq!(
        m.contents(),
        r#"{
    "foo": "$1bar"
}
"#
    );
}

#[test]
fn remove_main_key() {
    let mut m = JsonManipulator::new(
        r#"{
    "repositories": [
        {
            "package": {
                "require": {
                    "this/should-not-end-up-in-root-require": "~2.0"
                },
                "require-dev": {
                    "this/should-not-end-up-in-root-require-dev": "~2.0"
                }
            }
        }
    ],
    "require": {
        "package/a": "*",
        "package/b": "*",
        "package/c": "*"
    },
    "foo": "bar",
    "require-dev": {
        "package/d": "*"
    }
}"#,
    )
    .unwrap();
    assert!(m.remove_main_key("repositories"));
    assert_eq!(
        m.contents(),
        r#"{
    "require": {
        "package/a": "*",
        "package/b": "*",
        "package/c": "*"
    },
    "foo": "bar",
    "require-dev": {
        "package/d": "*"
    }
}
"#
    );
    assert!(m.remove_main_key("foo"));
    assert_eq!(
        m.contents(),
        r#"{
    "require": {
        "package/a": "*",
        "package/b": "*",
        "package/c": "*"
    },
    "require-dev": {
        "package/d": "*"
    }
}
"#
    );
    assert!(m.remove_main_key("require"));
    assert!(m.remove_main_key("require-dev"));
    assert_eq!(
        m.contents(),
        r#"{
}
"#
    );
}

#[test]
fn remove_main_key_if_empty() {
    let mut m = JsonManipulator::new(
        r#"{
    "repositories": [
    ],
    "require": {
        "package/a": "*",
        "package/b": "*",
        "package/c": "*"
    },
    "foo": "bar",
    "require-dev": {
    }
}"#,
    )
    .unwrap();
    assert!(m.remove_main_key_if_empty("repositories"));
    assert_eq!(
        m.contents(),
        r#"{
    "require": {
        "package/a": "*",
        "package/b": "*",
        "package/c": "*"
    },
    "foo": "bar",
    "require-dev": {
    }
}
"#
    );
    assert!(m.remove_main_key_if_empty("foo"));
    assert!(m.remove_main_key_if_empty("require"));
    assert!(m.remove_main_key_if_empty("require-dev"));
    assert_eq!(
        m.contents(),
        r#"{
    "require": {
        "package/a": "*",
        "package/b": "*",
        "package/c": "*"
    },
    "foo": "bar"
}
"#
    );
}

#[test]
fn remove_main_key_removes_key_where_value_is_null() {
    let mut m = JsonManipulator::new(r#"{"foo":9000,"bar":null}"#).unwrap();
    m.remove_main_key("bar");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&m.contents()).unwrap(), json!({"foo": 9000}));
}

#[test]
fn indent_detection() {
    let mut m = JsonManipulator::new(
        r#"{

  "require": {
    "php": "5.*"
  }
}"#,
    )
    .unwrap();
    assert!(m.add_main_key("require-dev", &json!({"foo": "qux"})));
    assert_eq!(
        m.contents(),
        r#"{

  "require": {
    "php": "5.*"
  },
  "require-dev": {
    "foo": "qux"
  }
}
"#
    );
}

#[test]
fn remove_main_key_at_end_of_file() {
    let mut m = JsonManipulator::new(
        r#"{
    "require": {
        "package/a": "*"
    }
}
"#,
    )
    .unwrap();
    assert!(m.add_main_key("homepage", &json!("http...")));
    assert!(m.add_main_key("license", &json!("mit")));
    assert_eq!(
        m.contents(),
        r#"{
    "require": {
        "package/a": "*"
    },
    "homepage": "http...",
    "license": "mit"
}
"#
    );
    assert!(m.remove_main_key("homepage"));
    assert!(m.remove_main_key("license"));
    assert_eq!(
        m.contents(),
        r#"{
    "require": {
        "package/a": "*"
    }
}
"#
    );
}

#[test]
fn escaped_unicode_does_not_cause_backtrack_limit_error_github_issue8131() {
    let mut m = JsonManipulator::new(
        r#"{
  "description": "Some U\u00F1icode",
  "require": {
    "foo/bar": "^1.0"
  }
}"#,
    )
    .unwrap();
    assert!(m.add_link("require", "foo/baz", "^1.0", false));
    assert_eq!(
        m.contents(),
        r#"{
  "description": "Some U\u00F1icode",
  "require": {
    "foo/bar": "^1.0",
    "foo/baz": "^1.0"
  }
}
"#
    );
}

#[test]
fn large_file_does_not_cause_backtrack_limit_error_github_issue9595() {
    let mut m = JsonManipulator::new(r#"{
    "name": "leoloso/pop",
    "require": {
        "php": "^7.4|^8.0",
        "ext-mbstring": "*",
        "brain/cortex": "~1.0.0",
        "composer/installers": "~1.0",
        "composer/semver": "^1.5",
        "erusev/parsedown": "^1.7",
        "guzzlehttp/guzzle": "~6.3",
        "jrfnl/php-cast-to-type": "^2.0",
        "league/pipeline": "^1.0",
        "lkwdwrd/wp-muplugin-loader": "dev-feature-composer-v2",
        "obsidian/polyfill-hrtime": "^0.1",
        "psr/cache": "^1.0",
        "symfony/cache": "^5.1",
        "symfony/config": "^5.1",
        "symfony/dependency-injection": "^5.1",
        "symfony/dotenv": "^5.1",
        "symfony/expression-language": "^5.1",
        "symfony/polyfill-php72": "^1.18",
        "symfony/polyfill-php73": "^1.18",
        "symfony/polyfill-php74": "^1.18",
        "symfony/polyfill-php80": "^1.18",
        "symfony/property-access": "^5.1",
        "symfony/yaml": "^5.1"
    },
    "require-dev": {
        "johnpbloch/wordpress": ">=5.5",
        "phpstan/phpstan": "^0.12",
        "phpunit/phpunit": ">=9.3",
        "rector/rector": "^0.9",
        "squizlabs/php_codesniffer": "^3.0",
        "symfony/var-dumper": "^5.1",
        "symplify/monorepo-builder": "^9.0",
        "szepeviktor/phpstan-wordpress": "^0.6.2"
    },
    "autoload": {
        "psr-4": {
            "GraphQLAPI\\ConvertCaseDirectives\\": "layers/GraphQLAPIForWP/plugins/convert-case-directives/src",
            "GraphQLAPI\\GraphQLAPI\\": "layers/GraphQLAPIForWP/plugins/graphql-api-for-wp/src",
            "GraphQLAPI\\SchemaFeedback\\": "layers/GraphQLAPIForWP/plugins/schema-feedback/src",
            "GraphQLByPoP\\GraphQLClientsForWP\\": "layers/GraphQLByPoP/packages/graphql-clients-for-wp/src",
            "GraphQLByPoP\\GraphQLEndpointForWP\\": "layers/GraphQLByPoP/packages/graphql-endpoint-for-wp/src",
            "GraphQLByPoP\\GraphQLParser\\": "layers/GraphQLByPoP/packages/graphql-parser/src",
            "GraphQLByPoP\\GraphQLQuery\\": "layers/GraphQLByPoP/packages/graphql-query/src",
            "GraphQLByPoP\\GraphQLRequest\\": "layers/GraphQLByPoP/packages/graphql-request/src",
            "GraphQLByPoP\\GraphQLServer\\": "layers/GraphQLByPoP/packages/graphql-server/src",
            "Leoloso\\ExamplesForPoP\\": "layers/Misc/packages/examples-for-pop/src",
            "PoPSchema\\BasicDirectives\\": "layers/Schema/packages/basic-directives/src",
            "PoPSchema\\BlockMetadataWP\\": "layers/Schema/packages/block-metadata-for-wp/src",
            "PoPSchema\\CDNDirective\\": "layers/Schema/packages/cdn-directive/src",
            "PoPSchema\\CategoriesWP\\": "layers/Schema/packages/categories-wp/src",
            "PoPSchema\\Categories\\": "layers/Schema/packages/categories/src",
            "PoPSchema\\CommentMetaWP\\": "layers/Schema/packages/commentmeta-wp/src",
            "PoPSchema\\CommentMeta\\": "layers/Schema/packages/commentmeta/src",
            "PoPSchema\\CommentMutationsWP\\": "layers/Schema/packages/comment-mutations-wp/src",
            "PoPSchema\\CommentMutations\\": "layers/Schema/packages/comment-mutations/src",
            "PoPSchema\\CommentsWP\\": "layers/Schema/packages/comments-wp/src",
            "PoPSchema\\Comments\\": "layers/Schema/packages/comments/src",
            "PoPSchema\\ConvertCaseDirectives\\": "layers/Schema/packages/convert-case-directives/src",
            "PoPSchema\\CustomPostMediaMutationsWP\\": "layers/Schema/packages/custompostmedia-mutations-wp/src",
            "PoPSchema\\CustomPostMediaMutations\\": "layers/Schema/packages/custompostmedia-mutations/src",
            "PoPSchema\\CustomPostMediaWP\\": "layers/Schema/packages/custompostmedia-wp/src",
            "PoPSchema\\CustomPostMedia\\": "layers/Schema/packages/custompostmedia/src",
            "PoPSchema\\CustomPostMetaWP\\": "layers/Schema/packages/custompostmeta-wp/src",
            "PoPSchema\\CustomPostMeta\\": "layers/Schema/packages/custompostmeta/src",
            "PoPSchema\\CustomPostMutationsWP\\": "layers/Schema/packages/custompost-mutations-wp/src",
            "PoPSchema\\CustomPostMutations\\": "layers/Schema/packages/custompost-mutations/src",
            "PoPSchema\\CustomPostsWP\\": "layers/Schema/packages/customposts-wp/src",
            "PoPSchema\\CustomPosts\\": "layers/Schema/packages/customposts/src",
            "PoPSchema\\EventMutationsWPEM\\": "layers/Schema/packages/event-mutations-wp-em/src",
            "PoPSchema\\EventMutations\\": "layers/Schema/packages/event-mutations/src",
            "PoPSchema\\EventsWPEM\\": "layers/Schema/packages/events-wp-em/src",
            "PoPSchema\\Events\\": "layers/Schema/packages/events/src",
            "PoPSchema\\EverythingElseWP\\": "layers/Schema/packages/everythingelse-wp/src",
            "PoPSchema\\EverythingElse\\": "layers/Schema/packages/everythingelse/src",
            "PoPSchema\\GenericCustomPosts\\": "layers/Schema/packages/generic-customposts/src",
            "PoPSchema\\GoogleTranslateDirectiveForCustomPosts\\": "layers/Schema/packages/google-translate-directive-for-customposts/src",
            "PoPSchema\\GoogleTranslateDirective\\": "layers/Schema/packages/google-translate-directive/src",
            "PoPSchema\\HighlightsWP\\": "layers/Schema/packages/highlights-wp/src",
            "PoPSchema\\Highlights\\": "layers/Schema/packages/highlights/src",
            "PoPSchema\\LocationPostsWP\\": "layers/Schema/packages/locationposts-wp/src",
            "PoPSchema\\LocationPosts\\": "layers/Schema/packages/locationposts/src",
            "PoPSchema\\LocationsWPEM\\": "layers/Schema/packages/locations-wp-em/src",
            "PoPSchema\\Locations\\": "layers/Schema/packages/locations/src",
            "PoPSchema\\MediaWP\\": "layers/Schema/packages/media-wp/src",
            "PoPSchema\\Media\\": "layers/Schema/packages/media/src",
            "PoPSchema\\MenusWP\\": "layers/Schema/packages/menus-wp/src",
            "PoPSchema\\Menus\\": "layers/Schema/packages/menus/src",
            "PoPSchema\\MetaQueryWP\\": "layers/Schema/packages/metaquery-wp/src",
            "PoPSchema\\MetaQuery\\": "layers/Schema/packages/metaquery/src",
            "PoPSchema\\Meta\\": "layers/Schema/packages/meta/src",
            "PoPSchema\\NotificationsWP\\": "layers/Schema/packages/notifications-wp/src",
            "PoPSchema\\Notifications\\": "layers/Schema/packages/notifications/src",
            "PoPSchema\\PagesWP\\": "layers/Schema/packages/pages-wp/src",
            "PoPSchema\\Pages\\": "layers/Schema/packages/pages/src",
            "PoPSchema\\PostMutations\\": "layers/Schema/packages/post-mutations/src",
            "PoPSchema\\PostTagsWP\\": "layers/Schema/packages/post-tags-wp/src",
            "PoPSchema\\PostTags\\": "layers/Schema/packages/post-tags/src",
            "PoPSchema\\PostsWP\\": "layers/Schema/packages/posts-wp/src",
            "PoPSchema\\Posts\\": "layers/Schema/packages/posts/src",
            "PoPSchema\\QueriedObjectWP\\": "layers/Schema/packages/queriedobject-wp/src",
            "PoPSchema\\QueriedObject\\": "layers/Schema/packages/queriedobject/src",
            "PoPSchema\\SchemaCommons\\": "layers/Schema/packages/schema-commons/src",
            "PoPSchema\\StancesWP\\": "layers/Schema/packages/stances-wp/src",
            "PoPSchema\\Stances\\": "layers/Schema/packages/stances/src",
            "PoPSchema\\TagsWP\\": "layers/Schema/packages/tags-wp/src",
            "PoPSchema\\Tags\\": "layers/Schema/packages/tags/src",
            "PoPSchema\\TaxonomiesWP\\": "layers/Schema/packages/taxonomies-wp/src",
            "PoPSchema\\Taxonomies\\": "layers/Schema/packages/taxonomies/src",
            "PoPSchema\\TaxonomyMetaWP\\": "layers/Schema/packages/taxonomymeta-wp/src",
            "PoPSchema\\TaxonomyMeta\\": "layers/Schema/packages/taxonomymeta/src",
            "PoPSchema\\TaxonomyQueryWP\\": "layers/Schema/packages/taxonomyquery-wp/src",
            "PoPSchema\\TaxonomyQuery\\": "layers/Schema/packages/taxonomyquery/src",
            "PoPSchema\\TranslateDirectiveACL\\": "layers/Schema/packages/translate-directive-acl/src",
            "PoPSchema\\TranslateDirective\\": "layers/Schema/packages/translate-directive/src",
            "PoPSchema\\UserMetaWP\\": "layers/Schema/packages/usermeta-wp/src",
            "PoPSchema\\UserMeta\\": "layers/Schema/packages/usermeta/src",
            "PoPSchema\\UserRolesACL\\": "layers/Schema/packages/user-roles-acl/src",
            "PoPSchema\\UserRolesAccessControl\\": "layers/Schema/packages/user-roles-access-control/src",
            "PoPSchema\\UserRolesWP\\": "layers/Schema/packages/user-roles-wp/src",
            "PoPSchema\\UserRoles\\": "layers/Schema/packages/user-roles/src",
            "PoPSchema\\UserStateAccessControl\\": "layers/Schema/packages/user-state-access-control/src",
            "PoPSchema\\UserStateMutationsWP\\": "layers/Schema/packages/user-state-mutations-wp/src",
            "PoPSchema\\UserStateMutations\\": "layers/Schema/packages/user-state-mutations/src",
            "PoPSchema\\UserStateWP\\": "layers/Schema/packages/user-state-wp/src",
            "PoPSchema\\UserState\\": "layers/Schema/packages/user-state/src",
            "PoPSchema\\UsersWP\\": "layers/Schema/packages/users-wp/src",
            "PoPSchema\\Users\\": "layers/Schema/packages/users/src",
            "PoPSitesWassup\\CommentMutations\\": "layers/Wassup/packages/comment-mutations/src",
            "PoPSitesWassup\\ContactUsMutations\\": "layers/Wassup/packages/contactus-mutations/src",
            "PoPSitesWassup\\ContactUserMutations\\": "layers/Wassup/packages/contactuser-mutations/src",
            "PoPSitesWassup\\CustomPostLinkMutations\\": "layers/Wassup/packages/custompostlink-mutations/src",
            "PoPSitesWassup\\CustomPostMutations\\": "layers/Wassup/packages/custompost-mutations/src",
            "PoPSitesWassup\\EventLinkMutations\\": "layers/Wassup/packages/eventlink-mutations/src",
            "PoPSitesWassup\\EventMutations\\": "layers/Wassup/packages/event-mutations/src",
            "PoPSitesWassup\\EverythingElseMutations\\": "layers/Wassup/packages/everythingelse-mutations/src",
            "PoPSitesWassup\\FlagMutations\\": "layers/Wassup/packages/flag-mutations/src",
            "PoPSitesWassup\\FormMutations\\": "layers/Wassup/packages/form-mutations/src",
            "PoPSitesWassup\\GravityFormsMutations\\": "layers/Wassup/packages/gravityforms-mutations/src",
            "PoPSitesWassup\\HighlightMutations\\": "layers/Wassup/packages/highlight-mutations/src",
            "PoPSitesWassup\\LocationMutations\\": "layers/Wassup/packages/location-mutations/src",
            "PoPSitesWassup\\LocationPostLinkMutations\\": "layers/Wassup/packages/locationpostlink-mutations/src",
            "PoPSitesWassup\\LocationPostMutations\\": "layers/Wassup/packages/locationpost-mutations/src",
            "PoPSitesWassup\\NewsletterMutations\\": "layers/Wassup/packages/newsletter-mutations/src",
            "PoPSitesWassup\\NotificationMutations\\": "layers/Wassup/packages/notification-mutations/src",
            "PoPSitesWassup\\PostLinkMutations\\": "layers/Wassup/packages/postlink-mutations/src",
            "PoPSitesWassup\\PostMutations\\": "layers/Wassup/packages/post-mutations/src",
            "PoPSitesWassup\\ShareMutations\\": "layers/Wassup/packages/share-mutations/src",
            "PoPSitesWassup\\SocialNetworkMutations\\": "layers/Wassup/packages/socialnetwork-mutations/src",
            "PoPSitesWassup\\StanceMutations\\": "layers/Wassup/packages/stance-mutations/src",
            "PoPSitesWassup\\SystemMutations\\": "layers/Wassup/packages/system-mutations/src",
            "PoPSitesWassup\\UserStateMutations\\": "layers/Wassup/packages/user-state-mutations/src",
            "PoPSitesWassup\\VolunteerMutations\\": "layers/Wassup/packages/volunteer-mutations/src",
            "PoPSitesWassup\\Wassup\\": "layers/Wassup/packages/wassup/src",
            "PoP\\APIClients\\": "layers/API/packages/api-clients/src",
            "PoP\\APIEndpointsForWP\\": "layers/API/packages/api-endpoints-for-wp/src",
            "PoP\\APIEndpoints\\": "layers/API/packages/api-endpoints/src",
            "PoP\\APIMirrorQuery\\": "layers/API/packages/api-mirrorquery/src",
            "PoP\\API\\": "layers/API/packages/api/src",
            "PoP\\AccessControl\\": "layers/Engine/packages/access-control/src",
            "PoP\\ApplicationWP\\": "layers/SiteBuilder/packages/application-wp/src",
            "PoP\\Application\\": "layers/SiteBuilder/packages/application/src",
            "PoP\\Base36Definitions\\": "layers/SiteBuilder/packages/definitions-base36/src",
            "PoP\\CacheControl\\": "layers/Engine/packages/cache-control/src",
            "PoP\\ComponentModel\\": "layers/Engine/packages/component-model/src",
            "PoP\\ConfigurableSchemaFeedback\\": "layers/Engine/packages/configurable-schema-feedback/src",
            "PoP\\ConfigurationComponentModel\\": "layers/SiteBuilder/packages/component-model-configuration/src",
            "PoP\\DefinitionPersistence\\": "layers/SiteBuilder/packages/definitionpersistence/src",
            "PoP\\Definitions\\": "layers/Engine/packages/definitions/src",
            "PoP\\EmojiDefinitions\\": "layers/SiteBuilder/packages/definitions-emoji/src",
            "PoP\\EngineWP\\": "layers/Engine/packages/engine-wp/src",
            "PoP\\Engine\\": "layers/Engine/packages/engine/src",
            "PoP\\FieldQuery\\": "layers/Engine/packages/field-query/src",
            "PoP\\FileStore\\": "layers/Engine/packages/filestore/src",
            "PoP\\FunctionFields\\": "layers/Engine/packages/function-fields/src",
            "PoP\\GraphQLAPI\\": "layers/API/packages/api-graphql/src",
            "PoP\\GuzzleHelpers\\": "layers/Engine/packages/guzzle-helpers/src",
            "PoP\\HooksWP\\": "layers/Engine/packages/hooks-wp/src",
            "PoP\\Hooks\\": "layers/Engine/packages/hooks/src",
            "PoP\\LooseContracts\\": "layers/Engine/packages/loosecontracts/src",
            "PoP\\MandatoryDirectivesByConfiguration\\": "layers/Engine/packages/mandatory-directives-by-configuration/src",
            "PoP\\ModuleRouting\\": "layers/Engine/packages/modulerouting/src",
            "PoP\\Multisite\\": "layers/SiteBuilder/packages/multisite/src",
            "PoP\\PoP\\": "src",
            "PoP\\QueryParsing\\": "layers/Engine/packages/query-parsing/src",
            "PoP\\RESTAPI\\": "layers/API/packages/api-rest/src",
            "PoP\\ResourceLoader\\": "layers/SiteBuilder/packages/resourceloader/src",
            "PoP\\Resources\\": "layers/SiteBuilder/packages/resources/src",
            "PoP\\Root\\": "layers/Engine/packages/root/src",
            "PoP\\RoutingWP\\": "layers/Engine/packages/routing-wp/src",
            "PoP\\Routing\\": "layers/Engine/packages/routing/src",
            "PoP\\SPA\\": "layers/SiteBuilder/packages/spa/src",
            "PoP\\SSG\\": "layers/SiteBuilder/packages/static-site-generator/src",
            "PoP\\SiteWP\\": "layers/SiteBuilder/packages/site-wp/src",
            "PoP\\Site\\": "layers/SiteBuilder/packages/site/src",
            "PoP\\TraceTools\\": "layers/Engine/packages/trace-tools/src",
            "PoP\\TranslationWP\\": "layers/Engine/packages/translation-wp/src",
            "PoP\\Translation\\": "layers/Engine/packages/translation/src"
        }
    },
    "autoload-dev": {
        "psr-4": {
            "GraphQLAPI\\ConvertCaseDirectives\\": "layers/GraphQLAPIForWP/plugins/convert-case-directives/tests",
            "GraphQLAPI\\GraphQLAPI\\": "layers/GraphQLAPIForWP/plugins/graphql-api-for-wp/tests",
            "GraphQLAPI\\SchemaFeedback\\": "layers/GraphQLAPIForWP/plugins/schema-feedback/tests",
            "GraphQLByPoP\\GraphQLClientsForWP\\": "layers/GraphQLByPoP/packages/graphql-clients-for-wp/tests",
            "GraphQLByPoP\\GraphQLEndpointForWP\\": "layers/GraphQLByPoP/packages/graphql-endpoint-for-wp/tests",
            "GraphQLByPoP\\GraphQLParser\\": "layers/GraphQLByPoP/packages/graphql-parser/tests",
            "GraphQLByPoP\\GraphQLQuery\\": "layers/GraphQLByPoP/packages/graphql-query/tests",
            "GraphQLByPoP\\GraphQLRequest\\": "layers/GraphQLByPoP/packages/graphql-request/tests",
            "GraphQLByPoP\\GraphQLServer\\": "layers/GraphQLByPoP/packages/graphql-server/tests",
            "Leoloso\\ExamplesForPoP\\": "layers/Misc/packages/examples-for-pop/tests",
            "PoPSchema\\BasicDirectives\\": "layers/Schema/packages/basic-directives/tests",
            "PoPSchema\\BlockMetadataWP\\": "layers/Schema/packages/block-metadata-for-wp/tests",
            "PoPSchema\\CDNDirective\\": "layers/Schema/packages/cdn-directive/tests",
            "PoPSchema\\CategoriesWP\\": "layers/Schema/packages/categories-wp/tests",
            "PoPSchema\\Categories\\": "layers/Schema/packages/categories/tests",
            "PoPSchema\\CommentMetaWP\\": "layers/Schema/packages/commentmeta-wp/tests",
            "PoPSchema\\CommentMeta\\": "layers/Schema/packages/commentmeta/tests",
            "PoPSchema\\CommentMutationsWP\\": "layers/Schema/packages/comment-mutations-wp/tests",
            "PoPSchema\\CommentMutations\\": "layers/Schema/packages/comment-mutations/tests",
            "PoPSchema\\CommentsWP\\": "layers/Schema/packages/comments-wp/tests",
            "PoPSchema\\Comments\\": "layers/Schema/packages/comments/tests",
            "PoPSchema\\ConvertCaseDirectives\\": "layers/Schema/packages/convert-case-directives/tests",
            "PoPSchema\\CustomPostMediaMutationsWP\\": "layers/Schema/packages/custompostmedia-mutations-wp/tests",
            "PoPSchema\\CustomPostMediaMutations\\": "layers/Schema/packages/custompostmedia-mutations/tests",
            "PoPSchema\\CustomPostMediaWP\\": "layers/Schema/packages/custompostmedia-wp/tests",
            "PoPSchema\\CustomPostMedia\\": "layers/Schema/packages/custompostmedia/tests",
            "PoPSchema\\CustomPostMetaWP\\": "layers/Schema/packages/custompostmeta-wp/tests",
            "PoPSchema\\CustomPostMeta\\": "layers/Schema/packages/custompostmeta/tests",
            "PoPSchema\\CustomPostMutationsWP\\": "layers/Schema/packages/custompost-mutations-wp/tests",
            "PoPSchema\\CustomPostMutations\\": "layers/Schema/packages/custompost-mutations/tests",
            "PoPSchema\\CustomPostsWP\\": "layers/Schema/packages/customposts-wp/tests",
            "PoPSchema\\CustomPosts\\": "layers/Schema/packages/customposts/tests",
            "PoPSchema\\EventMutationsWPEM\\": "layers/Schema/packages/event-mutations-wp-em/tests",
            "PoPSchema\\EventMutations\\": "layers/Schema/packages/event-mutations/tests",
            "PoPSchema\\EventsWPEM\\": "layers/Schema/packages/events-wp-em/tests",
            "PoPSchema\\Events\\": "layers/Schema/packages/events/tests",
            "PoPSchema\\EverythingElseWP\\": "layers/Schema/packages/everythingelse-wp/tests",
            "PoPSchema\\EverythingElse\\": "layers/Schema/packages/everythingelse/tests",
            "PoPSchema\\GenericCustomPosts\\": "layers/Schema/packages/generic-customposts/tests",
            "PoPSchema\\GoogleTranslateDirectiveForCustomPosts\\": "layers/Schema/packages/google-translate-directive-for-customposts/tests",
            "PoPSchema\\GoogleTranslateDirective\\": "layers/Schema/packages/google-translate-directive/tests",
            "PoPSchema\\HighlightsWP\\": "layers/Schema/packages/highlights-wp/tests",
            "PoPSchema\\Highlights\\": "layers/Schema/packages/highlights/tests",
            "PoPSchema\\LocationPostsWP\\": "layers/Schema/packages/locationposts-wp/tests",
            "PoPSchema\\LocationPosts\\": "layers/Schema/packages/locationposts/tests",
            "PoPSchema\\LocationsWPEM\\": "layers/Schema/packages/locations-wp-em/tests",
            "PoPSchema\\Locations\\": "layers/Schema/packages/locations/tests",
            "PoPSchema\\MediaWP\\": "layers/Schema/packages/media-wp/tests",
            "PoPSchema\\Media\\": "layers/Schema/packages/media/tests",
            "PoPSchema\\MenusWP\\": "layers/Schema/packages/menus-wp/tests",
            "PoPSchema\\Menus\\": "layers/Schema/packages/menus/tests",
            "PoPSchema\\MetaQueryWP\\": "layers/Schema/packages/metaquery-wp/tests",
            "PoPSchema\\MetaQuery\\": "layers/Schema/packages/metaquery/tests",
            "PoPSchema\\Meta\\": "layers/Schema/packages/meta/tests",
            "PoPSchema\\NotificationsWP\\": "layers/Schema/packages/notifications-wp/tests",
            "PoPSchema\\Notifications\\": "layers/Schema/packages/notifications/tests",
            "PoPSchema\\PagesWP\\": "layers/Schema/packages/pages-wp/tests",
            "PoPSchema\\Pages\\": "layers/Schema/packages/pages/tests",
            "PoPSchema\\PostMutations\\": "layers/Schema/packages/post-mutations/tests",
            "PoPSchema\\PostTagsWP\\": "layers/Schema/packages/post-tags-wp/tests",
            "PoPSchema\\PostTags\\": "layers/Schema/packages/post-tags/tests",
            "PoPSchema\\PostsWP\\": "layers/Schema/packages/posts-wp/tests",
            "PoPSchema\\Posts\\": "layers/Schema/packages/posts/tests",
            "PoPSchema\\QueriedObjectWP\\": "layers/Schema/packages/queriedobject-wp/tests",
            "PoPSchema\\QueriedObject\\": "layers/Schema/packages/queriedobject/tests",
            "PoPSchema\\SchemaCommons\\": "layers/Schema/packages/schema-commons/tests",
            "PoPSchema\\StancesWP\\": "layers/Schema/packages/stances-wp/tests",
            "PoPSchema\\Stances\\": "layers/Schema/packages/stances/tests",
            "PoPSchema\\TagsWP\\": "layers/Schema/packages/tags-wp/tests",
            "PoPSchema\\Tags\\": "layers/Schema/packages/tags/tests",
            "PoPSchema\\TaxonomiesWP\\": "layers/Schema/packages/taxonomies-wp/tests",
            "PoPSchema\\Taxonomies\\": "layers/Schema/packages/taxonomies/tests",
            "PoPSchema\\TaxonomyMetaWP\\": "layers/Schema/packages/taxonomymeta-wp/tests",
            "PoPSchema\\TaxonomyMeta\\": "layers/Schema/packages/taxonomymeta/tests",
            "PoPSchema\\TaxonomyQueryWP\\": "layers/Schema/packages/taxonomyquery-wp/tests",
            "PoPSchema\\TaxonomyQuery\\": "layers/Schema/packages/taxonomyquery/tests",
            "PoPSchema\\TranslateDirectiveACL\\": "layers/Schema/packages/translate-directive-acl/tests",
            "PoPSchema\\TranslateDirective\\": "layers/Schema/packages/translate-directive/tests",
            "PoPSchema\\UserMetaWP\\": "layers/Schema/packages/usermeta-wp/tests",
            "PoPSchema\\UserMeta\\": "layers/Schema/packages/usermeta/tests",
            "PoPSchema\\UserRolesACL\\": "layers/Schema/packages/user-roles-acl/tests",
            "PoPSchema\\UserRolesAccessControl\\": "layers/Schema/packages/user-roles-access-control/tests",
            "PoPSchema\\UserRolesWP\\": "layers/Schema/packages/user-roles-wp/tests",
            "PoPSchema\\UserRoles\\": "layers/Schema/packages/user-roles/tests",
            "PoPSchema\\UserStateAccessControl\\": "layers/Schema/packages/user-state-access-control/tests",
            "PoPSchema\\UserStateMutationsWP\\": "layers/Schema/packages/user-state-mutations-wp/tests",
            "PoPSchema\\UserStateMutations\\": "layers/Schema/packages/user-state-mutations/tests",
            "PoPSchema\\UserStateWP\\": "layers/Schema/packages/user-state-wp/tests",
            "PoPSchema\\UserState\\": "layers/Schema/packages/user-state/tests",
            "PoPSchema\\UsersWP\\": "layers/Schema/packages/users-wp/tests",
            "PoPSchema\\Users\\": "layers/Schema/packages/users/tests",
            "PoPSitesWassup\\CommentMutations\\": "layers/Wassup/packages/comment-mutations/tests",
            "PoPSitesWassup\\ContactUsMutations\\": "layers/Wassup/packages/contactus-mutations/tests",
            "PoPSitesWassup\\ContactUserMutations\\": "layers/Wassup/packages/contactuser-mutations/tests",
            "PoPSitesWassup\\CustomPostLinkMutations\\": "layers/Wassup/packages/custompostlink-mutations/tests",
            "PoPSitesWassup\\CustomPostMutations\\": "layers/Wassup/packages/custompost-mutations/tests",
            "PoPSitesWassup\\EventLinkMutations\\": "layers/Wassup/packages/eventlink-mutations/tests",
            "PoPSitesWassup\\EventMutations\\": "layers/Wassup/packages/event-mutations/tests",
            "PoPSitesWassup\\EverythingElseMutations\\": "layers/Wassup/packages/everythingelse-mutations/tests",
            "PoPSitesWassup\\FlagMutations\\": "layers/Wassup/packages/flag-mutations/tests",
            "PoPSitesWassup\\FormMutations\\": "layers/Wassup/packages/form-mutations/tests",
            "PoPSitesWassup\\GravityFormsMutations\\": "layers/Wassup/packages/gravityforms-mutations/tests",
            "PoPSitesWassup\\HighlightMutations\\": "layers/Wassup/packages/highlight-mutations/tests",
            "PoPSitesWassup\\LocationMutations\\": "layers/Wassup/packages/location-mutations/tests",
            "PoPSitesWassup\\LocationPostLinkMutations\\": "layers/Wassup/packages/locationpostlink-mutations/tests",
            "PoPSitesWassup\\LocationPostMutations\\": "layers/Wassup/packages/locationpost-mutations/tests",
            "PoPSitesWassup\\NewsletterMutations\\": "layers/Wassup/packages/newsletter-mutations/tests",
            "PoPSitesWassup\\NotificationMutations\\": "layers/Wassup/packages/notification-mutations/tests",
            "PoPSitesWassup\\PostLinkMutations\\": "layers/Wassup/packages/postlink-mutations/tests",
            "PoPSitesWassup\\PostMutations\\": "layers/Wassup/packages/post-mutations/tests",
            "PoPSitesWassup\\ShareMutations\\": "layers/Wassup/packages/share-mutations/tests",
            "PoPSitesWassup\\SocialNetworkMutations\\": "layers/Wassup/packages/socialnetwork-mutations/tests",
            "PoPSitesWassup\\StanceMutations\\": "layers/Wassup/packages/stance-mutations/tests",
            "PoPSitesWassup\\SystemMutations\\": "layers/Wassup/packages/system-mutations/tests",
            "PoPSitesWassup\\UserStateMutations\\": "layers/Wassup/packages/user-state-mutations/tests",
            "PoPSitesWassup\\VolunteerMutations\\": "layers/Wassup/packages/volunteer-mutations/tests",
            "PoPSitesWassup\\Wassup\\": "layers/Wassup/packages/wassup/tests",
            "PoP\\APIClients\\": "layers/API/packages/api-clients/tests",
            "PoP\\APIEndpointsForWP\\": "layers/API/packages/api-endpoints-for-wp/tests",
            "PoP\\APIEndpoints\\": "layers/API/packages/api-endpoints/tests",
            "PoP\\APIMirrorQuery\\": "layers/API/packages/api-mirrorquery/tests",
            "PoP\\API\\": "layers/API/packages/api/tests",
            "PoP\\AccessControl\\": "layers/Engine/packages/access-control/tests",
            "PoP\\ApplicationWP\\": "layers/SiteBuilder/packages/application-wp/tests",
            "PoP\\Application\\": "layers/SiteBuilder/packages/application/tests",
            "PoP\\Base36Definitions\\": "layers/SiteBuilder/packages/definitions-base36/tests",
            "PoP\\CacheControl\\": "layers/Engine/packages/cache-control/tests",
            "PoP\\ComponentModel\\": "layers/Engine/packages/component-model/tests",
            "PoP\\ConfigurableSchemaFeedback\\": "layers/Engine/packages/configurable-schema-feedback/tests",
            "PoP\\ConfigurationComponentModel\\": "layers/SiteBuilder/packages/component-model-configuration/tests",
            "PoP\\DefinitionPersistence\\": "layers/SiteBuilder/packages/definitionpersistence/tests",
            "PoP\\Definitions\\": "layers/Engine/packages/definitions/tests",
            "PoP\\EmojiDefinitions\\": "layers/SiteBuilder/packages/definitions-emoji/tests",
            "PoP\\EngineWP\\": "layers/Engine/packages/engine-wp/tests",
            "PoP\\Engine\\": "layers/Engine/packages/engine/tests",
            "PoP\\FieldQuery\\": "layers/Engine/packages/field-query/tests",
            "PoP\\FileStore\\": "layers/Engine/packages/filestore/tests",
            "PoP\\FunctionFields\\": "layers/Engine/packages/function-fields/tests",
            "PoP\\GraphQLAPI\\": "layers/API/packages/api-graphql/tests",
            "PoP\\GuzzleHelpers\\": "layers/Engine/packages/guzzle-helpers/tests",
            "PoP\\HooksWP\\": "layers/Engine/packages/hooks-wp/tests",
            "PoP\\Hooks\\": "layers/Engine/packages/hooks/tests",
            "PoP\\LooseContracts\\": "layers/Engine/packages/loosecontracts/tests",
            "PoP\\MandatoryDirectivesByConfiguration\\": "layers/Engine/packages/mandatory-directives-by-configuration/tests",
            "PoP\\ModuleRouting\\": "layers/Engine/packages/modulerouting/tests",
            "PoP\\Multisite\\": "layers/SiteBuilder/packages/multisite/tests",
            "PoP\\QueryParsing\\": "layers/Engine/packages/query-parsing/tests",
            "PoP\\RESTAPI\\": "layers/API/packages/api-rest/tests",
            "PoP\\ResourceLoader\\": "layers/SiteBuilder/packages/resourceloader/tests",
            "PoP\\Resources\\": "layers/SiteBuilder/packages/resources/tests",
            "PoP\\Root\\": "layers/Engine/packages/root/tests",
            "PoP\\RoutingWP\\": "layers/Engine/packages/routing-wp/tests",
            "PoP\\Routing\\": "layers/Engine/packages/routing/tests",
            "PoP\\SPA\\": "layers/SiteBuilder/packages/spa/tests",
            "PoP\\SSG\\": "layers/SiteBuilder/packages/static-site-generator/tests",
            "PoP\\SiteWP\\": "layers/SiteBuilder/packages/site-wp/tests",
            "PoP\\Site\\": "layers/SiteBuilder/packages/site/tests",
            "PoP\\TraceTools\\": "layers/Engine/packages/trace-tools/tests",
            "PoP\\TranslationWP\\": "layers/Engine/packages/translation-wp/tests",
            "PoP\\Translation\\": "layers/Engine/packages/translation/tests"
        }
    },
    "extra": {
        "wordpress-install-dir": "vendor/wordpress/wordpress",
        "merge-plugin": {
            "include": [
                "composer.local.json"
            ],
            "recurse": true,
            "replace": false,
            "ignore-duplicates": false,
            "merge-dev": true,
            "merge-extra": false,
            "merge-extra-deep": false,
            "merge-scripts": false
        }
    },
    "replace": {
        "getpop/access-control": "self.version",
        "getpop/api": "self.version",
        "getpop/api-clients": "self.version",
        "getpop/api-endpoints": "self.version",
        "getpop/api-endpoints-for-wp": "self.version",
        "getpop/api-graphql": "self.version",
        "getpop/api-mirrorquery": "self.version",
        "getpop/api-rest": "self.version",
        "getpop/application": "self.version",
        "getpop/application-wp": "self.version",
        "getpop/cache-control": "self.version",
        "getpop/component-model": "self.version",
        "getpop/component-model-configuration": "self.version",
        "getpop/configurable-schema-feedback": "self.version",
        "getpop/definitionpersistence": "self.version",
        "getpop/definitions": "self.version",
        "getpop/definitions-base36": "self.version",
        "getpop/definitions-emoji": "self.version",
        "getpop/engine": "self.version",
        "getpop/engine-wp": "self.version",
        "getpop/engine-wp-bootloader": "self.version",
        "getpop/field-query": "self.version",
        "getpop/filestore": "self.version",
        "getpop/function-fields": "self.version",
        "getpop/guzzle-helpers": "self.version",
        "getpop/hooks": "self.version",
        "getpop/hooks-wp": "self.version",
        "getpop/loosecontracts": "self.version",
        "getpop/mandatory-directives-by-configuration": "self.version",
        "getpop/migrate-api": "self.version",
        "getpop/migrate-api-graphql": "self.version",
        "getpop/migrate-component-model": "self.version",
        "getpop/migrate-component-model-configuration": "self.version",
        "getpop/migrate-engine": "self.version",
        "getpop/migrate-engine-wp": "self.version",
        "getpop/migrate-static-site-generator": "self.version",
        "getpop/modulerouting": "self.version",
        "getpop/multisite": "self.version",
        "getpop/query-parsing": "self.version",
        "getpop/resourceloader": "self.version",
        "getpop/resources": "self.version",
        "getpop/root": "self.version",
        "getpop/routing": "self.version",
        "getpop/routing-wp": "self.version",
        "getpop/site": "self.version",
        "getpop/site-wp": "self.version",
        "getpop/spa": "self.version",
        "getpop/static-site-generator": "self.version",
        "getpop/trace-tools": "self.version",
        "getpop/translation": "self.version",
        "getpop/translation-wp": "self.version",
        "graphql-api/convert-case-directives": "self.version",
        "graphql-api/graphql-api-for-wp": "self.version",
        "graphql-api/schema-feedback": "self.version",
        "graphql-by-pop/graphql-clients-for-wp": "self.version",
        "graphql-by-pop/graphql-endpoint-for-wp": "self.version",
        "graphql-by-pop/graphql-parser": "self.version",
        "graphql-by-pop/graphql-query": "self.version",
        "graphql-by-pop/graphql-request": "self.version",
        "graphql-by-pop/graphql-server": "self.version",
        "leoloso/examples-for-pop": "self.version",
        "pop-migrate-everythingelse/cssconverter": "self.version",
        "pop-migrate-everythingelse/ssr": "self.version",
        "pop-schema/basic-directives": "self.version",
        "pop-schema/block-metadata-for-wp": "self.version",
        "pop-schema/categories": "self.version",
        "pop-schema/categories-wp": "self.version",
        "pop-schema/cdn-directive": "self.version",
        "pop-schema/comment-mutations": "self.version",
        "pop-schema/comment-mutations-wp": "self.version",
        "pop-schema/commentmeta": "self.version",
        "pop-schema/commentmeta-wp": "self.version",
        "pop-schema/comments": "self.version",
        "pop-schema/comments-wp": "self.version",
        "pop-schema/convert-case-directives": "self.version",
        "pop-schema/custompost-mutations": "self.version",
        "pop-schema/custompost-mutations-wp": "self.version",
        "pop-schema/custompostmedia": "self.version",
        "pop-schema/custompostmedia-mutations": "self.version",
        "pop-schema/custompostmedia-mutations-wp": "self.version",
        "pop-schema/custompostmedia-wp": "self.version",
        "pop-schema/custompostmeta": "self.version",
        "pop-schema/custompostmeta-wp": "self.version",
        "pop-schema/customposts": "self.version",
        "pop-schema/customposts-wp": "self.version",
        "pop-schema/event-mutations": "self.version",
        "pop-schema/event-mutations-wp-em": "self.version",
        "pop-schema/events": "self.version",
        "pop-schema/events-wp-em": "self.version",
        "pop-schema/everythingelse": "self.version",
        "pop-schema/everythingelse-wp": "self.version",
        "pop-schema/generic-customposts": "self.version",
        "pop-schema/google-translate-directive": "self.version",
        "pop-schema/google-translate-directive-for-customposts": "self.version",
        "pop-schema/highlights": "self.version",
        "pop-schema/highlights-wp": "self.version",
        "pop-schema/locationposts": "self.version",
        "pop-schema/locationposts-wp": "self.version",
        "pop-schema/locations": "self.version",
        "pop-schema/locations-wp-em": "self.version",
        "pop-schema/media": "self.version",
        "pop-schema/media-wp": "self.version",
        "pop-schema/menus": "self.version",
        "pop-schema/menus-wp": "self.version",
        "pop-schema/meta": "self.version",
        "pop-schema/metaquery": "self.version",
        "pop-schema/metaquery-wp": "self.version",
        "pop-schema/migrate-categories": "self.version",
        "pop-schema/migrate-categories-wp": "self.version",
        "pop-schema/migrate-commentmeta": "self.version",
        "pop-schema/migrate-commentmeta-wp": "self.version",
        "pop-schema/migrate-comments": "self.version",
        "pop-schema/migrate-comments-wp": "self.version",
        "pop-schema/migrate-custompostmedia": "self.version",
        "pop-schema/migrate-custompostmedia-wp": "self.version",
        "pop-schema/migrate-custompostmeta": "self.version",
        "pop-schema/migrate-custompostmeta-wp": "self.version",
        "pop-schema/migrate-customposts": "self.version",
        "pop-schema/migrate-customposts-wp": "self.version",
        "pop-schema/migrate-events": "self.version",
        "pop-schema/migrate-events-wp-em": "self.version",
        "pop-schema/migrate-everythingelse": "self.version",
        "pop-schema/migrate-locations": "self.version",
        "pop-schema/migrate-locations-wp-em": "self.version",
        "pop-schema/migrate-media": "self.version",
        "pop-schema/migrate-media-wp": "self.version",
        "pop-schema/migrate-meta": "self.version",
        "pop-schema/migrate-metaquery": "self.version",
        "pop-schema/migrate-metaquery-wp": "self.version",
        "pop-schema/migrate-pages": "self.version",
        "pop-schema/migrate-pages-wp": "self.version",
        "pop-schema/migrate-post-tags": "self.version",
        "pop-schema/migrate-post-tags-wp": "self.version",
        "pop-schema/migrate-posts": "self.version",
        "pop-schema/migrate-posts-wp": "self.version",
        "pop-schema/migrate-queriedobject": "self.version",
        "pop-schema/migrate-queriedobject-wp": "self.version",
        "pop-schema/migrate-tags": "self.version",
        "pop-schema/migrate-tags-wp": "self.version",
        "pop-schema/migrate-taxonomies": "self.version",
        "pop-schema/migrate-taxonomies-wp": "self.version",
        "pop-schema/migrate-taxonomymeta": "self.version",
        "pop-schema/migrate-taxonomymeta-wp": "self.version",
        "pop-schema/migrate-taxonomyquery": "self.version",
        "pop-schema/migrate-taxonomyquery-wp": "self.version",
        "pop-schema/migrate-usermeta": "self.version",
        "pop-schema/migrate-usermeta-wp": "self.version",
        "pop-schema/migrate-users": "self.version",
        "pop-schema/migrate-users-wp": "self.version",
        "pop-schema/notifications": "self.version",
        "pop-schema/notifications-wp": "self.version",
        "pop-schema/pages": "self.version",
        "pop-schema/pages-wp": "self.version",
        "pop-schema/post-mutations": "self.version",
        "pop-schema/post-tags": "self.version",
        "pop-schema/post-tags-wp": "self.version",
        "pop-schema/posts": "self.version",
        "pop-schema/posts-wp": "self.version",
        "pop-schema/queriedobject": "self.version",
        "pop-schema/queriedobject-wp": "self.version",
        "pop-schema/schema-commons": "self.version",
        "pop-schema/stances": "self.version",
        "pop-schema/stances-wp": "self.version",
        "pop-schema/tags": "self.version",
        "pop-schema/tags-wp": "self.version",
        "pop-schema/taxonomies": "self.version",
        "pop-schema/taxonomies-wp": "self.version",
        "pop-schema/taxonomymeta": "self.version",
        "pop-schema/taxonomymeta-wp": "self.version",
        "pop-schema/taxonomyquery": "self.version",
        "pop-schema/taxonomyquery-wp": "self.version",
        "pop-schema/translate-directive": "self.version",
        "pop-schema/translate-directive-acl": "self.version",
        "pop-schema/user-roles": "self.version",
        "pop-schema/user-roles-access-control": "self.version",
        "pop-schema/user-roles-acl": "self.version",
        "pop-schema/user-roles-wp": "self.version",
        "pop-schema/user-state": "self.version",
        "pop-schema/user-state-access-control": "self.version",
        "pop-schema/user-state-mutations": "self.version",
        "pop-schema/user-state-mutations-wp": "self.version",
        "pop-schema/user-state-wp": "self.version",
        "pop-schema/usermeta": "self.version",
        "pop-schema/usermeta-wp": "self.version",
        "pop-schema/users": "self.version",
        "pop-schema/users-wp": "self.version",
        "pop-sites-wassup/comment-mutations": "self.version",
        "pop-sites-wassup/contactus-mutations": "self.version",
        "pop-sites-wassup/contactuser-mutations": "self.version",
        "pop-sites-wassup/custompost-mutations": "self.version",
        "pop-sites-wassup/custompostlink-mutations": "self.version",
        "pop-sites-wassup/event-mutations": "self.version",
        "pop-sites-wassup/eventlink-mutations": "self.version",
        "pop-sites-wassup/everythingelse-mutations": "self.version",
        "pop-sites-wassup/flag-mutations": "self.version",
        "pop-sites-wassup/form-mutations": "self.version",
        "pop-sites-wassup/gravityforms-mutations": "self.version",
        "pop-sites-wassup/highlight-mutations": "self.version",
        "pop-sites-wassup/location-mutations": "self.version",
        "pop-sites-wassup/locationpost-mutations": "self.version",
        "pop-sites-wassup/locationpostlink-mutations": "self.version",
        "pop-sites-wassup/newsletter-mutations": "self.version",
        "pop-sites-wassup/notification-mutations": "self.version",
        "pop-sites-wassup/post-mutations": "self.version",
        "pop-sites-wassup/postlink-mutations": "self.version",
        "pop-sites-wassup/share-mutations": "self.version",
        "pop-sites-wassup/socialnetwork-mutations": "self.version",
        "pop-sites-wassup/stance-mutations": "self.version",
        "pop-sites-wassup/system-mutations": "self.version",
        "pop-sites-wassup/user-state-mutations": "self.version",
        "pop-sites-wassup/volunteer-mutations": "self.version",
        "pop-sites-wassup/wassup": "self.version"
    },
    "authors": [
        {
            "name": "Leonardo Losoviz",
            "email": "leo@getpop.org",
            "homepage": "https://getpop.org"
        }
    ],
    "description": "Monorepo for all the PoP packages",
    "license": "GPL-2.0-or-later",
    "config": {
        "sort-packages": true
    },
    "repositories": [
        {
            "type": "composer",
            "url": "https://wpackagist.org"
        },
        {
            "type": "vcs",
            "url": "https://github.com/leoloso/wp-muplugin-loader.git"
        },
        {
            "type": "vcs",
            "url": "https://github.com/mcaskill/composer-merge-plugin.git"
        }
    ],
    "scripts": {
        "test": "phpunit",
        "check-style": "phpcs -n src $(monorepo-builder source-packages --subfolder=src --subfolder=tests)",
        "fix-style": "phpcbf -n src $(monorepo-builder source-packages --subfolder=src --subfolder=tests)",
        "analyse": "ci/phpstan.sh \". $(monorepo-builder source-packages --skip-unmigrated)\"",
        "preview-src-downgrade": "rector process $(monorepo-builder source-packages --subfolder=src) --config=rector-downgrade-code.php --ansi --dry-run || true",
        "preview-vendor-downgrade": "layers/Engine/packages/root/ci/downgrade_code.sh 7.1 rector-downgrade-code.php --dry-run || true",
        "preview-code-downgrade": [
            "@preview-src-downgrade",
            "@preview-vendor-downgrade"
        ],
        "build-server": [
            "lando init --source remote --remote-url https://wordpress.org/latest.tar.gz --recipe wordpress --webroot wordpress --name graphql-api-dev",
            "@start-server"
        ],
        "start-server": [
            "cd layers/GraphQLAPIForWP/plugins/graphql-api-for-wp && composer install",
            "lando start"
        ],
        "rebuild-server": "lando rebuild -y",
        "merge-monorepo": "monorepo-builder merge --ansi",
        "propagate-monorepo": "monorepo-builder propagate --ansi",
        "validate-monorepo": "monorepo-builder validate --ansi",
        "release": "monorepo-builder release patch --ansi"
    },
    "minimum-stability": "dev",
    "prefer-stable": true
}"#).unwrap();
    assert!(m.add_sub_node("config", "platform-check", &json!(false), true));
    assert_eq!(
        m.contents(),
        r#"{
    "name": "leoloso/pop",
    "require": {
        "php": "^7.4|^8.0",
        "ext-mbstring": "*",
        "brain/cortex": "~1.0.0",
        "composer/installers": "~1.0",
        "composer/semver": "^1.5",
        "erusev/parsedown": "^1.7",
        "guzzlehttp/guzzle": "~6.3",
        "jrfnl/php-cast-to-type": "^2.0",
        "league/pipeline": "^1.0",
        "lkwdwrd/wp-muplugin-loader": "dev-feature-composer-v2",
        "obsidian/polyfill-hrtime": "^0.1",
        "psr/cache": "^1.0",
        "symfony/cache": "^5.1",
        "symfony/config": "^5.1",
        "symfony/dependency-injection": "^5.1",
        "symfony/dotenv": "^5.1",
        "symfony/expression-language": "^5.1",
        "symfony/polyfill-php72": "^1.18",
        "symfony/polyfill-php73": "^1.18",
        "symfony/polyfill-php74": "^1.18",
        "symfony/polyfill-php80": "^1.18",
        "symfony/property-access": "^5.1",
        "symfony/yaml": "^5.1"
    },
    "require-dev": {
        "johnpbloch/wordpress": ">=5.5",
        "phpstan/phpstan": "^0.12",
        "phpunit/phpunit": ">=9.3",
        "rector/rector": "^0.9",
        "squizlabs/php_codesniffer": "^3.0",
        "symfony/var-dumper": "^5.1",
        "symplify/monorepo-builder": "^9.0",
        "szepeviktor/phpstan-wordpress": "^0.6.2"
    },
    "autoload": {
        "psr-4": {
            "GraphQLAPI\\ConvertCaseDirectives\\": "layers/GraphQLAPIForWP/plugins/convert-case-directives/src",
            "GraphQLAPI\\GraphQLAPI\\": "layers/GraphQLAPIForWP/plugins/graphql-api-for-wp/src",
            "GraphQLAPI\\SchemaFeedback\\": "layers/GraphQLAPIForWP/plugins/schema-feedback/src",
            "GraphQLByPoP\\GraphQLClientsForWP\\": "layers/GraphQLByPoP/packages/graphql-clients-for-wp/src",
            "GraphQLByPoP\\GraphQLEndpointForWP\\": "layers/GraphQLByPoP/packages/graphql-endpoint-for-wp/src",
            "GraphQLByPoP\\GraphQLParser\\": "layers/GraphQLByPoP/packages/graphql-parser/src",
            "GraphQLByPoP\\GraphQLQuery\\": "layers/GraphQLByPoP/packages/graphql-query/src",
            "GraphQLByPoP\\GraphQLRequest\\": "layers/GraphQLByPoP/packages/graphql-request/src",
            "GraphQLByPoP\\GraphQLServer\\": "layers/GraphQLByPoP/packages/graphql-server/src",
            "Leoloso\\ExamplesForPoP\\": "layers/Misc/packages/examples-for-pop/src",
            "PoPSchema\\BasicDirectives\\": "layers/Schema/packages/basic-directives/src",
            "PoPSchema\\BlockMetadataWP\\": "layers/Schema/packages/block-metadata-for-wp/src",
            "PoPSchema\\CDNDirective\\": "layers/Schema/packages/cdn-directive/src",
            "PoPSchema\\CategoriesWP\\": "layers/Schema/packages/categories-wp/src",
            "PoPSchema\\Categories\\": "layers/Schema/packages/categories/src",
            "PoPSchema\\CommentMetaWP\\": "layers/Schema/packages/commentmeta-wp/src",
            "PoPSchema\\CommentMeta\\": "layers/Schema/packages/commentmeta/src",
            "PoPSchema\\CommentMutationsWP\\": "layers/Schema/packages/comment-mutations-wp/src",
            "PoPSchema\\CommentMutations\\": "layers/Schema/packages/comment-mutations/src",
            "PoPSchema\\CommentsWP\\": "layers/Schema/packages/comments-wp/src",
            "PoPSchema\\Comments\\": "layers/Schema/packages/comments/src",
            "PoPSchema\\ConvertCaseDirectives\\": "layers/Schema/packages/convert-case-directives/src",
            "PoPSchema\\CustomPostMediaMutationsWP\\": "layers/Schema/packages/custompostmedia-mutations-wp/src",
            "PoPSchema\\CustomPostMediaMutations\\": "layers/Schema/packages/custompostmedia-mutations/src",
            "PoPSchema\\CustomPostMediaWP\\": "layers/Schema/packages/custompostmedia-wp/src",
            "PoPSchema\\CustomPostMedia\\": "layers/Schema/packages/custompostmedia/src",
            "PoPSchema\\CustomPostMetaWP\\": "layers/Schema/packages/custompostmeta-wp/src",
            "PoPSchema\\CustomPostMeta\\": "layers/Schema/packages/custompostmeta/src",
            "PoPSchema\\CustomPostMutationsWP\\": "layers/Schema/packages/custompost-mutations-wp/src",
            "PoPSchema\\CustomPostMutations\\": "layers/Schema/packages/custompost-mutations/src",
            "PoPSchema\\CustomPostsWP\\": "layers/Schema/packages/customposts-wp/src",
            "PoPSchema\\CustomPosts\\": "layers/Schema/packages/customposts/src",
            "PoPSchema\\EventMutationsWPEM\\": "layers/Schema/packages/event-mutations-wp-em/src",
            "PoPSchema\\EventMutations\\": "layers/Schema/packages/event-mutations/src",
            "PoPSchema\\EventsWPEM\\": "layers/Schema/packages/events-wp-em/src",
            "PoPSchema\\Events\\": "layers/Schema/packages/events/src",
            "PoPSchema\\EverythingElseWP\\": "layers/Schema/packages/everythingelse-wp/src",
            "PoPSchema\\EverythingElse\\": "layers/Schema/packages/everythingelse/src",
            "PoPSchema\\GenericCustomPosts\\": "layers/Schema/packages/generic-customposts/src",
            "PoPSchema\\GoogleTranslateDirectiveForCustomPosts\\": "layers/Schema/packages/google-translate-directive-for-customposts/src",
            "PoPSchema\\GoogleTranslateDirective\\": "layers/Schema/packages/google-translate-directive/src",
            "PoPSchema\\HighlightsWP\\": "layers/Schema/packages/highlights-wp/src",
            "PoPSchema\\Highlights\\": "layers/Schema/packages/highlights/src",
            "PoPSchema\\LocationPostsWP\\": "layers/Schema/packages/locationposts-wp/src",
            "PoPSchema\\LocationPosts\\": "layers/Schema/packages/locationposts/src",
            "PoPSchema\\LocationsWPEM\\": "layers/Schema/packages/locations-wp-em/src",
            "PoPSchema\\Locations\\": "layers/Schema/packages/locations/src",
            "PoPSchema\\MediaWP\\": "layers/Schema/packages/media-wp/src",
            "PoPSchema\\Media\\": "layers/Schema/packages/media/src",
            "PoPSchema\\MenusWP\\": "layers/Schema/packages/menus-wp/src",
            "PoPSchema\\Menus\\": "layers/Schema/packages/menus/src",
            "PoPSchema\\MetaQueryWP\\": "layers/Schema/packages/metaquery-wp/src",
            "PoPSchema\\MetaQuery\\": "layers/Schema/packages/metaquery/src",
            "PoPSchema\\Meta\\": "layers/Schema/packages/meta/src",
            "PoPSchema\\NotificationsWP\\": "layers/Schema/packages/notifications-wp/src",
            "PoPSchema\\Notifications\\": "layers/Schema/packages/notifications/src",
            "PoPSchema\\PagesWP\\": "layers/Schema/packages/pages-wp/src",
            "PoPSchema\\Pages\\": "layers/Schema/packages/pages/src",
            "PoPSchema\\PostMutations\\": "layers/Schema/packages/post-mutations/src",
            "PoPSchema\\PostTagsWP\\": "layers/Schema/packages/post-tags-wp/src",
            "PoPSchema\\PostTags\\": "layers/Schema/packages/post-tags/src",
            "PoPSchema\\PostsWP\\": "layers/Schema/packages/posts-wp/src",
            "PoPSchema\\Posts\\": "layers/Schema/packages/posts/src",
            "PoPSchema\\QueriedObjectWP\\": "layers/Schema/packages/queriedobject-wp/src",
            "PoPSchema\\QueriedObject\\": "layers/Schema/packages/queriedobject/src",
            "PoPSchema\\SchemaCommons\\": "layers/Schema/packages/schema-commons/src",
            "PoPSchema\\StancesWP\\": "layers/Schema/packages/stances-wp/src",
            "PoPSchema\\Stances\\": "layers/Schema/packages/stances/src",
            "PoPSchema\\TagsWP\\": "layers/Schema/packages/tags-wp/src",
            "PoPSchema\\Tags\\": "layers/Schema/packages/tags/src",
            "PoPSchema\\TaxonomiesWP\\": "layers/Schema/packages/taxonomies-wp/src",
            "PoPSchema\\Taxonomies\\": "layers/Schema/packages/taxonomies/src",
            "PoPSchema\\TaxonomyMetaWP\\": "layers/Schema/packages/taxonomymeta-wp/src",
            "PoPSchema\\TaxonomyMeta\\": "layers/Schema/packages/taxonomymeta/src",
            "PoPSchema\\TaxonomyQueryWP\\": "layers/Schema/packages/taxonomyquery-wp/src",
            "PoPSchema\\TaxonomyQuery\\": "layers/Schema/packages/taxonomyquery/src",
            "PoPSchema\\TranslateDirectiveACL\\": "layers/Schema/packages/translate-directive-acl/src",
            "PoPSchema\\TranslateDirective\\": "layers/Schema/packages/translate-directive/src",
            "PoPSchema\\UserMetaWP\\": "layers/Schema/packages/usermeta-wp/src",
            "PoPSchema\\UserMeta\\": "layers/Schema/packages/usermeta/src",
            "PoPSchema\\UserRolesACL\\": "layers/Schema/packages/user-roles-acl/src",
            "PoPSchema\\UserRolesAccessControl\\": "layers/Schema/packages/user-roles-access-control/src",
            "PoPSchema\\UserRolesWP\\": "layers/Schema/packages/user-roles-wp/src",
            "PoPSchema\\UserRoles\\": "layers/Schema/packages/user-roles/src",
            "PoPSchema\\UserStateAccessControl\\": "layers/Schema/packages/user-state-access-control/src",
            "PoPSchema\\UserStateMutationsWP\\": "layers/Schema/packages/user-state-mutations-wp/src",
            "PoPSchema\\UserStateMutations\\": "layers/Schema/packages/user-state-mutations/src",
            "PoPSchema\\UserStateWP\\": "layers/Schema/packages/user-state-wp/src",
            "PoPSchema\\UserState\\": "layers/Schema/packages/user-state/src",
            "PoPSchema\\UsersWP\\": "layers/Schema/packages/users-wp/src",
            "PoPSchema\\Users\\": "layers/Schema/packages/users/src",
            "PoPSitesWassup\\CommentMutations\\": "layers/Wassup/packages/comment-mutations/src",
            "PoPSitesWassup\\ContactUsMutations\\": "layers/Wassup/packages/contactus-mutations/src",
            "PoPSitesWassup\\ContactUserMutations\\": "layers/Wassup/packages/contactuser-mutations/src",
            "PoPSitesWassup\\CustomPostLinkMutations\\": "layers/Wassup/packages/custompostlink-mutations/src",
            "PoPSitesWassup\\CustomPostMutations\\": "layers/Wassup/packages/custompost-mutations/src",
            "PoPSitesWassup\\EventLinkMutations\\": "layers/Wassup/packages/eventlink-mutations/src",
            "PoPSitesWassup\\EventMutations\\": "layers/Wassup/packages/event-mutations/src",
            "PoPSitesWassup\\EverythingElseMutations\\": "layers/Wassup/packages/everythingelse-mutations/src",
            "PoPSitesWassup\\FlagMutations\\": "layers/Wassup/packages/flag-mutations/src",
            "PoPSitesWassup\\FormMutations\\": "layers/Wassup/packages/form-mutations/src",
            "PoPSitesWassup\\GravityFormsMutations\\": "layers/Wassup/packages/gravityforms-mutations/src",
            "PoPSitesWassup\\HighlightMutations\\": "layers/Wassup/packages/highlight-mutations/src",
            "PoPSitesWassup\\LocationMutations\\": "layers/Wassup/packages/location-mutations/src",
            "PoPSitesWassup\\LocationPostLinkMutations\\": "layers/Wassup/packages/locationpostlink-mutations/src",
            "PoPSitesWassup\\LocationPostMutations\\": "layers/Wassup/packages/locationpost-mutations/src",
            "PoPSitesWassup\\NewsletterMutations\\": "layers/Wassup/packages/newsletter-mutations/src",
            "PoPSitesWassup\\NotificationMutations\\": "layers/Wassup/packages/notification-mutations/src",
            "PoPSitesWassup\\PostLinkMutations\\": "layers/Wassup/packages/postlink-mutations/src",
            "PoPSitesWassup\\PostMutations\\": "layers/Wassup/packages/post-mutations/src",
            "PoPSitesWassup\\ShareMutations\\": "layers/Wassup/packages/share-mutations/src",
            "PoPSitesWassup\\SocialNetworkMutations\\": "layers/Wassup/packages/socialnetwork-mutations/src",
            "PoPSitesWassup\\StanceMutations\\": "layers/Wassup/packages/stance-mutations/src",
            "PoPSitesWassup\\SystemMutations\\": "layers/Wassup/packages/system-mutations/src",
            "PoPSitesWassup\\UserStateMutations\\": "layers/Wassup/packages/user-state-mutations/src",
            "PoPSitesWassup\\VolunteerMutations\\": "layers/Wassup/packages/volunteer-mutations/src",
            "PoPSitesWassup\\Wassup\\": "layers/Wassup/packages/wassup/src",
            "PoP\\APIClients\\": "layers/API/packages/api-clients/src",
            "PoP\\APIEndpointsForWP\\": "layers/API/packages/api-endpoints-for-wp/src",
            "PoP\\APIEndpoints\\": "layers/API/packages/api-endpoints/src",
            "PoP\\APIMirrorQuery\\": "layers/API/packages/api-mirrorquery/src",
            "PoP\\API\\": "layers/API/packages/api/src",
            "PoP\\AccessControl\\": "layers/Engine/packages/access-control/src",
            "PoP\\ApplicationWP\\": "layers/SiteBuilder/packages/application-wp/src",
            "PoP\\Application\\": "layers/SiteBuilder/packages/application/src",
            "PoP\\Base36Definitions\\": "layers/SiteBuilder/packages/definitions-base36/src",
            "PoP\\CacheControl\\": "layers/Engine/packages/cache-control/src",
            "PoP\\ComponentModel\\": "layers/Engine/packages/component-model/src",
            "PoP\\ConfigurableSchemaFeedback\\": "layers/Engine/packages/configurable-schema-feedback/src",
            "PoP\\ConfigurationComponentModel\\": "layers/SiteBuilder/packages/component-model-configuration/src",
            "PoP\\DefinitionPersistence\\": "layers/SiteBuilder/packages/definitionpersistence/src",
            "PoP\\Definitions\\": "layers/Engine/packages/definitions/src",
            "PoP\\EmojiDefinitions\\": "layers/SiteBuilder/packages/definitions-emoji/src",
            "PoP\\EngineWP\\": "layers/Engine/packages/engine-wp/src",
            "PoP\\Engine\\": "layers/Engine/packages/engine/src",
            "PoP\\FieldQuery\\": "layers/Engine/packages/field-query/src",
            "PoP\\FileStore\\": "layers/Engine/packages/filestore/src",
            "PoP\\FunctionFields\\": "layers/Engine/packages/function-fields/src",
            "PoP\\GraphQLAPI\\": "layers/API/packages/api-graphql/src",
            "PoP\\GuzzleHelpers\\": "layers/Engine/packages/guzzle-helpers/src",
            "PoP\\HooksWP\\": "layers/Engine/packages/hooks-wp/src",
            "PoP\\Hooks\\": "layers/Engine/packages/hooks/src",
            "PoP\\LooseContracts\\": "layers/Engine/packages/loosecontracts/src",
            "PoP\\MandatoryDirectivesByConfiguration\\": "layers/Engine/packages/mandatory-directives-by-configuration/src",
            "PoP\\ModuleRouting\\": "layers/Engine/packages/modulerouting/src",
            "PoP\\Multisite\\": "layers/SiteBuilder/packages/multisite/src",
            "PoP\\PoP\\": "src",
            "PoP\\QueryParsing\\": "layers/Engine/packages/query-parsing/src",
            "PoP\\RESTAPI\\": "layers/API/packages/api-rest/src",
            "PoP\\ResourceLoader\\": "layers/SiteBuilder/packages/resourceloader/src",
            "PoP\\Resources\\": "layers/SiteBuilder/packages/resources/src",
            "PoP\\Root\\": "layers/Engine/packages/root/src",
            "PoP\\RoutingWP\\": "layers/Engine/packages/routing-wp/src",
            "PoP\\Routing\\": "layers/Engine/packages/routing/src",
            "PoP\\SPA\\": "layers/SiteBuilder/packages/spa/src",
            "PoP\\SSG\\": "layers/SiteBuilder/packages/static-site-generator/src",
            "PoP\\SiteWP\\": "layers/SiteBuilder/packages/site-wp/src",
            "PoP\\Site\\": "layers/SiteBuilder/packages/site/src",
            "PoP\\TraceTools\\": "layers/Engine/packages/trace-tools/src",
            "PoP\\TranslationWP\\": "layers/Engine/packages/translation-wp/src",
            "PoP\\Translation\\": "layers/Engine/packages/translation/src"
        }
    },
    "autoload-dev": {
        "psr-4": {
            "GraphQLAPI\\ConvertCaseDirectives\\": "layers/GraphQLAPIForWP/plugins/convert-case-directives/tests",
            "GraphQLAPI\\GraphQLAPI\\": "layers/GraphQLAPIForWP/plugins/graphql-api-for-wp/tests",
            "GraphQLAPI\\SchemaFeedback\\": "layers/GraphQLAPIForWP/plugins/schema-feedback/tests",
            "GraphQLByPoP\\GraphQLClientsForWP\\": "layers/GraphQLByPoP/packages/graphql-clients-for-wp/tests",
            "GraphQLByPoP\\GraphQLEndpointForWP\\": "layers/GraphQLByPoP/packages/graphql-endpoint-for-wp/tests",
            "GraphQLByPoP\\GraphQLParser\\": "layers/GraphQLByPoP/packages/graphql-parser/tests",
            "GraphQLByPoP\\GraphQLQuery\\": "layers/GraphQLByPoP/packages/graphql-query/tests",
            "GraphQLByPoP\\GraphQLRequest\\": "layers/GraphQLByPoP/packages/graphql-request/tests",
            "GraphQLByPoP\\GraphQLServer\\": "layers/GraphQLByPoP/packages/graphql-server/tests",
            "Leoloso\\ExamplesForPoP\\": "layers/Misc/packages/examples-for-pop/tests",
            "PoPSchema\\BasicDirectives\\": "layers/Schema/packages/basic-directives/tests",
            "PoPSchema\\BlockMetadataWP\\": "layers/Schema/packages/block-metadata-for-wp/tests",
            "PoPSchema\\CDNDirective\\": "layers/Schema/packages/cdn-directive/tests",
            "PoPSchema\\CategoriesWP\\": "layers/Schema/packages/categories-wp/tests",
            "PoPSchema\\Categories\\": "layers/Schema/packages/categories/tests",
            "PoPSchema\\CommentMetaWP\\": "layers/Schema/packages/commentmeta-wp/tests",
            "PoPSchema\\CommentMeta\\": "layers/Schema/packages/commentmeta/tests",
            "PoPSchema\\CommentMutationsWP\\": "layers/Schema/packages/comment-mutations-wp/tests",
            "PoPSchema\\CommentMutations\\": "layers/Schema/packages/comment-mutations/tests",
            "PoPSchema\\CommentsWP\\": "layers/Schema/packages/comments-wp/tests",
            "PoPSchema\\Comments\\": "layers/Schema/packages/comments/tests",
            "PoPSchema\\ConvertCaseDirectives\\": "layers/Schema/packages/convert-case-directives/tests",
            "PoPSchema\\CustomPostMediaMutationsWP\\": "layers/Schema/packages/custompostmedia-mutations-wp/tests",
            "PoPSchema\\CustomPostMediaMutations\\": "layers/Schema/packages/custompostmedia-mutations/tests",
            "PoPSchema\\CustomPostMediaWP\\": "layers/Schema/packages/custompostmedia-wp/tests",
            "PoPSchema\\CustomPostMedia\\": "layers/Schema/packages/custompostmedia/tests",
            "PoPSchema\\CustomPostMetaWP\\": "layers/Schema/packages/custompostmeta-wp/tests",
            "PoPSchema\\CustomPostMeta\\": "layers/Schema/packages/custompostmeta/tests",
            "PoPSchema\\CustomPostMutationsWP\\": "layers/Schema/packages/custompost-mutations-wp/tests",
            "PoPSchema\\CustomPostMutations\\": "layers/Schema/packages/custompost-mutations/tests",
            "PoPSchema\\CustomPostsWP\\": "layers/Schema/packages/customposts-wp/tests",
            "PoPSchema\\CustomPosts\\": "layers/Schema/packages/customposts/tests",
            "PoPSchema\\EventMutationsWPEM\\": "layers/Schema/packages/event-mutations-wp-em/tests",
            "PoPSchema\\EventMutations\\": "layers/Schema/packages/event-mutations/tests",
            "PoPSchema\\EventsWPEM\\": "layers/Schema/packages/events-wp-em/tests",
            "PoPSchema\\Events\\": "layers/Schema/packages/events/tests",
            "PoPSchema\\EverythingElseWP\\": "layers/Schema/packages/everythingelse-wp/tests",
            "PoPSchema\\EverythingElse\\": "layers/Schema/packages/everythingelse/tests",
            "PoPSchema\\GenericCustomPosts\\": "layers/Schema/packages/generic-customposts/tests",
            "PoPSchema\\GoogleTranslateDirectiveForCustomPosts\\": "layers/Schema/packages/google-translate-directive-for-customposts/tests",
            "PoPSchema\\GoogleTranslateDirective\\": "layers/Schema/packages/google-translate-directive/tests",
            "PoPSchema\\HighlightsWP\\": "layers/Schema/packages/highlights-wp/tests",
            "PoPSchema\\Highlights\\": "layers/Schema/packages/highlights/tests",
            "PoPSchema\\LocationPostsWP\\": "layers/Schema/packages/locationposts-wp/tests",
            "PoPSchema\\LocationPosts\\": "layers/Schema/packages/locationposts/tests",
            "PoPSchema\\LocationsWPEM\\": "layers/Schema/packages/locations-wp-em/tests",
            "PoPSchema\\Locations\\": "layers/Schema/packages/locations/tests",
            "PoPSchema\\MediaWP\\": "layers/Schema/packages/media-wp/tests",
            "PoPSchema\\Media\\": "layers/Schema/packages/media/tests",
            "PoPSchema\\MenusWP\\": "layers/Schema/packages/menus-wp/tests",
            "PoPSchema\\Menus\\": "layers/Schema/packages/menus/tests",
            "PoPSchema\\MetaQueryWP\\": "layers/Schema/packages/metaquery-wp/tests",
            "PoPSchema\\MetaQuery\\": "layers/Schema/packages/metaquery/tests",
            "PoPSchema\\Meta\\": "layers/Schema/packages/meta/tests",
            "PoPSchema\\NotificationsWP\\": "layers/Schema/packages/notifications-wp/tests",
            "PoPSchema\\Notifications\\": "layers/Schema/packages/notifications/tests",
            "PoPSchema\\PagesWP\\": "layers/Schema/packages/pages-wp/tests",
            "PoPSchema\\Pages\\": "layers/Schema/packages/pages/tests",
            "PoPSchema\\PostMutations\\": "layers/Schema/packages/post-mutations/tests",
            "PoPSchema\\PostTagsWP\\": "layers/Schema/packages/post-tags-wp/tests",
            "PoPSchema\\PostTags\\": "layers/Schema/packages/post-tags/tests",
            "PoPSchema\\PostsWP\\": "layers/Schema/packages/posts-wp/tests",
            "PoPSchema\\Posts\\": "layers/Schema/packages/posts/tests",
            "PoPSchema\\QueriedObjectWP\\": "layers/Schema/packages/queriedobject-wp/tests",
            "PoPSchema\\QueriedObject\\": "layers/Schema/packages/queriedobject/tests",
            "PoPSchema\\SchemaCommons\\": "layers/Schema/packages/schema-commons/tests",
            "PoPSchema\\StancesWP\\": "layers/Schema/packages/stances-wp/tests",
            "PoPSchema\\Stances\\": "layers/Schema/packages/stances/tests",
            "PoPSchema\\TagsWP\\": "layers/Schema/packages/tags-wp/tests",
            "PoPSchema\\Tags\\": "layers/Schema/packages/tags/tests",
            "PoPSchema\\TaxonomiesWP\\": "layers/Schema/packages/taxonomies-wp/tests",
            "PoPSchema\\Taxonomies\\": "layers/Schema/packages/taxonomies/tests",
            "PoPSchema\\TaxonomyMetaWP\\": "layers/Schema/packages/taxonomymeta-wp/tests",
            "PoPSchema\\TaxonomyMeta\\": "layers/Schema/packages/taxonomymeta/tests",
            "PoPSchema\\TaxonomyQueryWP\\": "layers/Schema/packages/taxonomyquery-wp/tests",
            "PoPSchema\\TaxonomyQuery\\": "layers/Schema/packages/taxonomyquery/tests",
            "PoPSchema\\TranslateDirectiveACL\\": "layers/Schema/packages/translate-directive-acl/tests",
            "PoPSchema\\TranslateDirective\\": "layers/Schema/packages/translate-directive/tests",
            "PoPSchema\\UserMetaWP\\": "layers/Schema/packages/usermeta-wp/tests",
            "PoPSchema\\UserMeta\\": "layers/Schema/packages/usermeta/tests",
            "PoPSchema\\UserRolesACL\\": "layers/Schema/packages/user-roles-acl/tests",
            "PoPSchema\\UserRolesAccessControl\\": "layers/Schema/packages/user-roles-access-control/tests",
            "PoPSchema\\UserRolesWP\\": "layers/Schema/packages/user-roles-wp/tests",
            "PoPSchema\\UserRoles\\": "layers/Schema/packages/user-roles/tests",
            "PoPSchema\\UserStateAccessControl\\": "layers/Schema/packages/user-state-access-control/tests",
            "PoPSchema\\UserStateMutationsWP\\": "layers/Schema/packages/user-state-mutations-wp/tests",
            "PoPSchema\\UserStateMutations\\": "layers/Schema/packages/user-state-mutations/tests",
            "PoPSchema\\UserStateWP\\": "layers/Schema/packages/user-state-wp/tests",
            "PoPSchema\\UserState\\": "layers/Schema/packages/user-state/tests",
            "PoPSchema\\UsersWP\\": "layers/Schema/packages/users-wp/tests",
            "PoPSchema\\Users\\": "layers/Schema/packages/users/tests",
            "PoPSitesWassup\\CommentMutations\\": "layers/Wassup/packages/comment-mutations/tests",
            "PoPSitesWassup\\ContactUsMutations\\": "layers/Wassup/packages/contactus-mutations/tests",
            "PoPSitesWassup\\ContactUserMutations\\": "layers/Wassup/packages/contactuser-mutations/tests",
            "PoPSitesWassup\\CustomPostLinkMutations\\": "layers/Wassup/packages/custompostlink-mutations/tests",
            "PoPSitesWassup\\CustomPostMutations\\": "layers/Wassup/packages/custompost-mutations/tests",
            "PoPSitesWassup\\EventLinkMutations\\": "layers/Wassup/packages/eventlink-mutations/tests",
            "PoPSitesWassup\\EventMutations\\": "layers/Wassup/packages/event-mutations/tests",
            "PoPSitesWassup\\EverythingElseMutations\\": "layers/Wassup/packages/everythingelse-mutations/tests",
            "PoPSitesWassup\\FlagMutations\\": "layers/Wassup/packages/flag-mutations/tests",
            "PoPSitesWassup\\FormMutations\\": "layers/Wassup/packages/form-mutations/tests",
            "PoPSitesWassup\\GravityFormsMutations\\": "layers/Wassup/packages/gravityforms-mutations/tests",
            "PoPSitesWassup\\HighlightMutations\\": "layers/Wassup/packages/highlight-mutations/tests",
            "PoPSitesWassup\\LocationMutations\\": "layers/Wassup/packages/location-mutations/tests",
            "PoPSitesWassup\\LocationPostLinkMutations\\": "layers/Wassup/packages/locationpostlink-mutations/tests",
            "PoPSitesWassup\\LocationPostMutations\\": "layers/Wassup/packages/locationpost-mutations/tests",
            "PoPSitesWassup\\NewsletterMutations\\": "layers/Wassup/packages/newsletter-mutations/tests",
            "PoPSitesWassup\\NotificationMutations\\": "layers/Wassup/packages/notification-mutations/tests",
            "PoPSitesWassup\\PostLinkMutations\\": "layers/Wassup/packages/postlink-mutations/tests",
            "PoPSitesWassup\\PostMutations\\": "layers/Wassup/packages/post-mutations/tests",
            "PoPSitesWassup\\ShareMutations\\": "layers/Wassup/packages/share-mutations/tests",
            "PoPSitesWassup\\SocialNetworkMutations\\": "layers/Wassup/packages/socialnetwork-mutations/tests",
            "PoPSitesWassup\\StanceMutations\\": "layers/Wassup/packages/stance-mutations/tests",
            "PoPSitesWassup\\SystemMutations\\": "layers/Wassup/packages/system-mutations/tests",
            "PoPSitesWassup\\UserStateMutations\\": "layers/Wassup/packages/user-state-mutations/tests",
            "PoPSitesWassup\\VolunteerMutations\\": "layers/Wassup/packages/volunteer-mutations/tests",
            "PoPSitesWassup\\Wassup\\": "layers/Wassup/packages/wassup/tests",
            "PoP\\APIClients\\": "layers/API/packages/api-clients/tests",
            "PoP\\APIEndpointsForWP\\": "layers/API/packages/api-endpoints-for-wp/tests",
            "PoP\\APIEndpoints\\": "layers/API/packages/api-endpoints/tests",
            "PoP\\APIMirrorQuery\\": "layers/API/packages/api-mirrorquery/tests",
            "PoP\\API\\": "layers/API/packages/api/tests",
            "PoP\\AccessControl\\": "layers/Engine/packages/access-control/tests",
            "PoP\\ApplicationWP\\": "layers/SiteBuilder/packages/application-wp/tests",
            "PoP\\Application\\": "layers/SiteBuilder/packages/application/tests",
            "PoP\\Base36Definitions\\": "layers/SiteBuilder/packages/definitions-base36/tests",
            "PoP\\CacheControl\\": "layers/Engine/packages/cache-control/tests",
            "PoP\\ComponentModel\\": "layers/Engine/packages/component-model/tests",
            "PoP\\ConfigurableSchemaFeedback\\": "layers/Engine/packages/configurable-schema-feedback/tests",
            "PoP\\ConfigurationComponentModel\\": "layers/SiteBuilder/packages/component-model-configuration/tests",
            "PoP\\DefinitionPersistence\\": "layers/SiteBuilder/packages/definitionpersistence/tests",
            "PoP\\Definitions\\": "layers/Engine/packages/definitions/tests",
            "PoP\\EmojiDefinitions\\": "layers/SiteBuilder/packages/definitions-emoji/tests",
            "PoP\\EngineWP\\": "layers/Engine/packages/engine-wp/tests",
            "PoP\\Engine\\": "layers/Engine/packages/engine/tests",
            "PoP\\FieldQuery\\": "layers/Engine/packages/field-query/tests",
            "PoP\\FileStore\\": "layers/Engine/packages/filestore/tests",
            "PoP\\FunctionFields\\": "layers/Engine/packages/function-fields/tests",
            "PoP\\GraphQLAPI\\": "layers/API/packages/api-graphql/tests",
            "PoP\\GuzzleHelpers\\": "layers/Engine/packages/guzzle-helpers/tests",
            "PoP\\HooksWP\\": "layers/Engine/packages/hooks-wp/tests",
            "PoP\\Hooks\\": "layers/Engine/packages/hooks/tests",
            "PoP\\LooseContracts\\": "layers/Engine/packages/loosecontracts/tests",
            "PoP\\MandatoryDirectivesByConfiguration\\": "layers/Engine/packages/mandatory-directives-by-configuration/tests",
            "PoP\\ModuleRouting\\": "layers/Engine/packages/modulerouting/tests",
            "PoP\\Multisite\\": "layers/SiteBuilder/packages/multisite/tests",
            "PoP\\QueryParsing\\": "layers/Engine/packages/query-parsing/tests",
            "PoP\\RESTAPI\\": "layers/API/packages/api-rest/tests",
            "PoP\\ResourceLoader\\": "layers/SiteBuilder/packages/resourceloader/tests",
            "PoP\\Resources\\": "layers/SiteBuilder/packages/resources/tests",
            "PoP\\Root\\": "layers/Engine/packages/root/tests",
            "PoP\\RoutingWP\\": "layers/Engine/packages/routing-wp/tests",
            "PoP\\Routing\\": "layers/Engine/packages/routing/tests",
            "PoP\\SPA\\": "layers/SiteBuilder/packages/spa/tests",
            "PoP\\SSG\\": "layers/SiteBuilder/packages/static-site-generator/tests",
            "PoP\\SiteWP\\": "layers/SiteBuilder/packages/site-wp/tests",
            "PoP\\Site\\": "layers/SiteBuilder/packages/site/tests",
            "PoP\\TraceTools\\": "layers/Engine/packages/trace-tools/tests",
            "PoP\\TranslationWP\\": "layers/Engine/packages/translation-wp/tests",
            "PoP\\Translation\\": "layers/Engine/packages/translation/tests"
        }
    },
    "extra": {
        "wordpress-install-dir": "vendor/wordpress/wordpress",
        "merge-plugin": {
            "include": [
                "composer.local.json"
            ],
            "recurse": true,
            "replace": false,
            "ignore-duplicates": false,
            "merge-dev": true,
            "merge-extra": false,
            "merge-extra-deep": false,
            "merge-scripts": false
        }
    },
    "replace": {
        "getpop/access-control": "self.version",
        "getpop/api": "self.version",
        "getpop/api-clients": "self.version",
        "getpop/api-endpoints": "self.version",
        "getpop/api-endpoints-for-wp": "self.version",
        "getpop/api-graphql": "self.version",
        "getpop/api-mirrorquery": "self.version",
        "getpop/api-rest": "self.version",
        "getpop/application": "self.version",
        "getpop/application-wp": "self.version",
        "getpop/cache-control": "self.version",
        "getpop/component-model": "self.version",
        "getpop/component-model-configuration": "self.version",
        "getpop/configurable-schema-feedback": "self.version",
        "getpop/definitionpersistence": "self.version",
        "getpop/definitions": "self.version",
        "getpop/definitions-base36": "self.version",
        "getpop/definitions-emoji": "self.version",
        "getpop/engine": "self.version",
        "getpop/engine-wp": "self.version",
        "getpop/engine-wp-bootloader": "self.version",
        "getpop/field-query": "self.version",
        "getpop/filestore": "self.version",
        "getpop/function-fields": "self.version",
        "getpop/guzzle-helpers": "self.version",
        "getpop/hooks": "self.version",
        "getpop/hooks-wp": "self.version",
        "getpop/loosecontracts": "self.version",
        "getpop/mandatory-directives-by-configuration": "self.version",
        "getpop/migrate-api": "self.version",
        "getpop/migrate-api-graphql": "self.version",
        "getpop/migrate-component-model": "self.version",
        "getpop/migrate-component-model-configuration": "self.version",
        "getpop/migrate-engine": "self.version",
        "getpop/migrate-engine-wp": "self.version",
        "getpop/migrate-static-site-generator": "self.version",
        "getpop/modulerouting": "self.version",
        "getpop/multisite": "self.version",
        "getpop/query-parsing": "self.version",
        "getpop/resourceloader": "self.version",
        "getpop/resources": "self.version",
        "getpop/root": "self.version",
        "getpop/routing": "self.version",
        "getpop/routing-wp": "self.version",
        "getpop/site": "self.version",
        "getpop/site-wp": "self.version",
        "getpop/spa": "self.version",
        "getpop/static-site-generator": "self.version",
        "getpop/trace-tools": "self.version",
        "getpop/translation": "self.version",
        "getpop/translation-wp": "self.version",
        "graphql-api/convert-case-directives": "self.version",
        "graphql-api/graphql-api-for-wp": "self.version",
        "graphql-api/schema-feedback": "self.version",
        "graphql-by-pop/graphql-clients-for-wp": "self.version",
        "graphql-by-pop/graphql-endpoint-for-wp": "self.version",
        "graphql-by-pop/graphql-parser": "self.version",
        "graphql-by-pop/graphql-query": "self.version",
        "graphql-by-pop/graphql-request": "self.version",
        "graphql-by-pop/graphql-server": "self.version",
        "leoloso/examples-for-pop": "self.version",
        "pop-migrate-everythingelse/cssconverter": "self.version",
        "pop-migrate-everythingelse/ssr": "self.version",
        "pop-schema/basic-directives": "self.version",
        "pop-schema/block-metadata-for-wp": "self.version",
        "pop-schema/categories": "self.version",
        "pop-schema/categories-wp": "self.version",
        "pop-schema/cdn-directive": "self.version",
        "pop-schema/comment-mutations": "self.version",
        "pop-schema/comment-mutations-wp": "self.version",
        "pop-schema/commentmeta": "self.version",
        "pop-schema/commentmeta-wp": "self.version",
        "pop-schema/comments": "self.version",
        "pop-schema/comments-wp": "self.version",
        "pop-schema/convert-case-directives": "self.version",
        "pop-schema/custompost-mutations": "self.version",
        "pop-schema/custompost-mutations-wp": "self.version",
        "pop-schema/custompostmedia": "self.version",
        "pop-schema/custompostmedia-mutations": "self.version",
        "pop-schema/custompostmedia-mutations-wp": "self.version",
        "pop-schema/custompostmedia-wp": "self.version",
        "pop-schema/custompostmeta": "self.version",
        "pop-schema/custompostmeta-wp": "self.version",
        "pop-schema/customposts": "self.version",
        "pop-schema/customposts-wp": "self.version",
        "pop-schema/event-mutations": "self.version",
        "pop-schema/event-mutations-wp-em": "self.version",
        "pop-schema/events": "self.version",
        "pop-schema/events-wp-em": "self.version",
        "pop-schema/everythingelse": "self.version",
        "pop-schema/everythingelse-wp": "self.version",
        "pop-schema/generic-customposts": "self.version",
        "pop-schema/google-translate-directive": "self.version",
        "pop-schema/google-translate-directive-for-customposts": "self.version",
        "pop-schema/highlights": "self.version",
        "pop-schema/highlights-wp": "self.version",
        "pop-schema/locationposts": "self.version",
        "pop-schema/locationposts-wp": "self.version",
        "pop-schema/locations": "self.version",
        "pop-schema/locations-wp-em": "self.version",
        "pop-schema/media": "self.version",
        "pop-schema/media-wp": "self.version",
        "pop-schema/menus": "self.version",
        "pop-schema/menus-wp": "self.version",
        "pop-schema/meta": "self.version",
        "pop-schema/metaquery": "self.version",
        "pop-schema/metaquery-wp": "self.version",
        "pop-schema/migrate-categories": "self.version",
        "pop-schema/migrate-categories-wp": "self.version",
        "pop-schema/migrate-commentmeta": "self.version",
        "pop-schema/migrate-commentmeta-wp": "self.version",
        "pop-schema/migrate-comments": "self.version",
        "pop-schema/migrate-comments-wp": "self.version",
        "pop-schema/migrate-custompostmedia": "self.version",
        "pop-schema/migrate-custompostmedia-wp": "self.version",
        "pop-schema/migrate-custompostmeta": "self.version",
        "pop-schema/migrate-custompostmeta-wp": "self.version",
        "pop-schema/migrate-customposts": "self.version",
        "pop-schema/migrate-customposts-wp": "self.version",
        "pop-schema/migrate-events": "self.version",
        "pop-schema/migrate-events-wp-em": "self.version",
        "pop-schema/migrate-everythingelse": "self.version",
        "pop-schema/migrate-locations": "self.version",
        "pop-schema/migrate-locations-wp-em": "self.version",
        "pop-schema/migrate-media": "self.version",
        "pop-schema/migrate-media-wp": "self.version",
        "pop-schema/migrate-meta": "self.version",
        "pop-schema/migrate-metaquery": "self.version",
        "pop-schema/migrate-metaquery-wp": "self.version",
        "pop-schema/migrate-pages": "self.version",
        "pop-schema/migrate-pages-wp": "self.version",
        "pop-schema/migrate-post-tags": "self.version",
        "pop-schema/migrate-post-tags-wp": "self.version",
        "pop-schema/migrate-posts": "self.version",
        "pop-schema/migrate-posts-wp": "self.version",
        "pop-schema/migrate-queriedobject": "self.version",
        "pop-schema/migrate-queriedobject-wp": "self.version",
        "pop-schema/migrate-tags": "self.version",
        "pop-schema/migrate-tags-wp": "self.version",
        "pop-schema/migrate-taxonomies": "self.version",
        "pop-schema/migrate-taxonomies-wp": "self.version",
        "pop-schema/migrate-taxonomymeta": "self.version",
        "pop-schema/migrate-taxonomymeta-wp": "self.version",
        "pop-schema/migrate-taxonomyquery": "self.version",
        "pop-schema/migrate-taxonomyquery-wp": "self.version",
        "pop-schema/migrate-usermeta": "self.version",
        "pop-schema/migrate-usermeta-wp": "self.version",
        "pop-schema/migrate-users": "self.version",
        "pop-schema/migrate-users-wp": "self.version",
        "pop-schema/notifications": "self.version",
        "pop-schema/notifications-wp": "self.version",
        "pop-schema/pages": "self.version",
        "pop-schema/pages-wp": "self.version",
        "pop-schema/post-mutations": "self.version",
        "pop-schema/post-tags": "self.version",
        "pop-schema/post-tags-wp": "self.version",
        "pop-schema/posts": "self.version",
        "pop-schema/posts-wp": "self.version",
        "pop-schema/queriedobject": "self.version",
        "pop-schema/queriedobject-wp": "self.version",
        "pop-schema/schema-commons": "self.version",
        "pop-schema/stances": "self.version",
        "pop-schema/stances-wp": "self.version",
        "pop-schema/tags": "self.version",
        "pop-schema/tags-wp": "self.version",
        "pop-schema/taxonomies": "self.version",
        "pop-schema/taxonomies-wp": "self.version",
        "pop-schema/taxonomymeta": "self.version",
        "pop-schema/taxonomymeta-wp": "self.version",
        "pop-schema/taxonomyquery": "self.version",
        "pop-schema/taxonomyquery-wp": "self.version",
        "pop-schema/translate-directive": "self.version",
        "pop-schema/translate-directive-acl": "self.version",
        "pop-schema/user-roles": "self.version",
        "pop-schema/user-roles-access-control": "self.version",
        "pop-schema/user-roles-acl": "self.version",
        "pop-schema/user-roles-wp": "self.version",
        "pop-schema/user-state": "self.version",
        "pop-schema/user-state-access-control": "self.version",
        "pop-schema/user-state-mutations": "self.version",
        "pop-schema/user-state-mutations-wp": "self.version",
        "pop-schema/user-state-wp": "self.version",
        "pop-schema/usermeta": "self.version",
        "pop-schema/usermeta-wp": "self.version",
        "pop-schema/users": "self.version",
        "pop-schema/users-wp": "self.version",
        "pop-sites-wassup/comment-mutations": "self.version",
        "pop-sites-wassup/contactus-mutations": "self.version",
        "pop-sites-wassup/contactuser-mutations": "self.version",
        "pop-sites-wassup/custompost-mutations": "self.version",
        "pop-sites-wassup/custompostlink-mutations": "self.version",
        "pop-sites-wassup/event-mutations": "self.version",
        "pop-sites-wassup/eventlink-mutations": "self.version",
        "pop-sites-wassup/everythingelse-mutations": "self.version",
        "pop-sites-wassup/flag-mutations": "self.version",
        "pop-sites-wassup/form-mutations": "self.version",
        "pop-sites-wassup/gravityforms-mutations": "self.version",
        "pop-sites-wassup/highlight-mutations": "self.version",
        "pop-sites-wassup/location-mutations": "self.version",
        "pop-sites-wassup/locationpost-mutations": "self.version",
        "pop-sites-wassup/locationpostlink-mutations": "self.version",
        "pop-sites-wassup/newsletter-mutations": "self.version",
        "pop-sites-wassup/notification-mutations": "self.version",
        "pop-sites-wassup/post-mutations": "self.version",
        "pop-sites-wassup/postlink-mutations": "self.version",
        "pop-sites-wassup/share-mutations": "self.version",
        "pop-sites-wassup/socialnetwork-mutations": "self.version",
        "pop-sites-wassup/stance-mutations": "self.version",
        "pop-sites-wassup/system-mutations": "self.version",
        "pop-sites-wassup/user-state-mutations": "self.version",
        "pop-sites-wassup/volunteer-mutations": "self.version",
        "pop-sites-wassup/wassup": "self.version"
    },
    "authors": [
        {
            "name": "Leonardo Losoviz",
            "email": "leo@getpop.org",
            "homepage": "https://getpop.org"
        }
    ],
    "description": "Monorepo for all the PoP packages",
    "license": "GPL-2.0-or-later",
    "config": {
        "sort-packages": true,
        "platform-check": false
    },
    "repositories": [
        {
            "type": "composer",
            "url": "https://wpackagist.org"
        },
        {
            "type": "vcs",
            "url": "https://github.com/leoloso/wp-muplugin-loader.git"
        },
        {
            "type": "vcs",
            "url": "https://github.com/mcaskill/composer-merge-plugin.git"
        }
    ],
    "scripts": {
        "test": "phpunit",
        "check-style": "phpcs -n src $(monorepo-builder source-packages --subfolder=src --subfolder=tests)",
        "fix-style": "phpcbf -n src $(monorepo-builder source-packages --subfolder=src --subfolder=tests)",
        "analyse": "ci/phpstan.sh \". $(monorepo-builder source-packages --skip-unmigrated)\"",
        "preview-src-downgrade": "rector process $(monorepo-builder source-packages --subfolder=src) --config=rector-downgrade-code.php --ansi --dry-run || true",
        "preview-vendor-downgrade": "layers/Engine/packages/root/ci/downgrade_code.sh 7.1 rector-downgrade-code.php --dry-run || true",
        "preview-code-downgrade": [
            "@preview-src-downgrade",
            "@preview-vendor-downgrade"
        ],
        "build-server": [
            "lando init --source remote --remote-url https://wordpress.org/latest.tar.gz --recipe wordpress --webroot wordpress --name graphql-api-dev",
            "@start-server"
        ],
        "start-server": [
            "cd layers/GraphQLAPIForWP/plugins/graphql-api-for-wp && composer install",
            "lando start"
        ],
        "rebuild-server": "lando rebuild -y",
        "merge-monorepo": "monorepo-builder merge --ansi",
        "propagate-monorepo": "monorepo-builder propagate --ansi",
        "validate-monorepo": "monorepo-builder validate --ansi",
        "release": "monorepo-builder release patch --ansi"
    },
    "minimum-stability": "dev",
    "prefer-stable": true
}
"#
    );
}

const ADD_REPOSITORY: &[(&str, &str, &str, &str, &str, bool)] = &[
    (
        "prepend repository (list)",
        r#"{
    "repositories": [
        {
            "type": "git",
            "url": "example.tld"
        }
    ]
}
"#,
        r#"{
    "repositories": [
        {
            "type": "path",
            "url": "foo/bar"
        },
        {
            "type": "git",
            "url": "example.tld"
        }
    ]
}
"#,
        "",
        r#"{"type":"path","url":"foo/bar"}"#,
        false,
    ),
    (
        "append repository (list)",
        r#"{
    "repositories": [
        {
            "type": "git",
            "url": "example.tld"
        }
    ]
}
"#,
        r#"{
    "repositories": [
        {
            "type": "git",
            "url": "example.tld"
        },
        {
            "type": "path",
            "url": "foo/bar"
        }
    ]
}
"#,
        "",
        r#"{"type":"path","url":"foo/bar"}"#,
        true,
    ),
    (
        "prepend repository by name (assoc)",
        r#"{
    "repositories": {
        "0": {
            "type": "git",
            "url": "example.tld"
        },
        "packagist.org": false
    }
}
"#,
        r#"{
    "repositories": [
        {
            "name": "foo",
            "type": "path",
            "url": "foo/bar"
        },
        {
            "type": "git",
            "url": "example.tld"
        },
        {
            "packagist.org": false
        }
    ]
}
"#,
        "foo",
        r#"{"type":"path","url":"foo/bar"}"#,
        false,
    ),
    (
        "append repository by name (assoc)",
        r#"{
    "repositories": {
        "0": {
            "type": "git",
            "url": "example.tld"
        },
        "packagist.org": false
    }
}
"#,
        r#"{
    "repositories": [
        {
            "type": "git",
            "url": "example.tld"
        },
        {
            "packagist.org": false
        },
        {
            "name": "foo",
            "type": "path",
            "url": "foo/bar"
        }
    ]
}
"#,
        "foo",
        r#"{"type":"path","url":"foo/bar"}"#,
        true,
    ),
    (
        "prepend repository by name (list to assoc)",
        r#"{
    "repositories": [
        {
            "type": "git",
            "url": "example.tld"
        }
    ]
}
"#,
        r#"{
    "repositories": [
        {
            "name": "foo",
            "type": "path",
            "url": "foo/bar"
        },
        {
            "type": "git",
            "url": "example.tld"
        }
    ]
}
"#,
        "foo",
        r#"{"type":"path","url":"foo/bar"}"#,
        false,
    ),
    (
        "append repository by name (list to assoc)",
        r#"{
    "repositories": [
        {
            "type": "git",
            "url": "example.tld"
        }
    ]
}
"#,
        r#"{
    "repositories": [
        {
            "type": "git",
            "url": "example.tld"
        },
        {
            "name": "foo",
            "type": "path",
            "url": "foo/bar"
        }
    ]
}
"#,
        "foo",
        r#"{"type":"path","url":"foo/bar"}"#,
        true,
    ),
    (
        "prepend repository (assoc)",
        r#"{
    "repositories": {
        "0": {
            "type": "git",
            "url": "example.tld"
        },
        "packagist.org": false
    }
}
"#,
        r#"{
    "repositories": [
        {
            "type": "path",
            "url": "foo/bar"
        },
        {
            "type": "git",
            "url": "example.tld"
        },
        {
            "packagist.org": false
        }
    ]
}
"#,
        "",
        r#"{"type":"path","url":"foo/bar"}"#,
        false,
    ),
    (
        "append repository (assoc)",
        r#"{
    "repositories": {
        "0": {
            "type": "git",
            "url": "example.tld"
        },
        "packagist.org": false
    }
}
"#,
        r#"{
    "repositories": [
        {
            "type": "git",
            "url": "example.tld"
        },
        {
            "packagist.org": false
        },
        {
            "type": "path",
            "url": "foo/bar"
        }
    ]
}
"#,
        "",
        r#"{"type":"path","url":"foo/bar"}"#,
        true,
    ),
];

const SET_REPOSITORY_URL: &[(&str, &str, &str, &str, &str)] = &[
    (
        "change first of three (assoc)",
        r#"{
    "repositories": {
        "first": {
            "type": "package",
            "url": "https://first.test"
        },
        "foo": {
            "type": "vcs",
            "url": "https://old.example.org"
        },
        "bar": {
            "type": "vcs",
            "url": "https://other.example.org"
        }
    }
}
"#,
        r#"{
    "repositories": {
        "first": {
            "type": "package",
            "url": "https://new.example.org"
        },
        "foo": {
            "type": "vcs",
            "url": "https://old.example.org"
        },
        "bar": {
            "type": "vcs",
            "url": "https://other.example.org"
        }
    }
}
"#,
        "first",
        "https://new.example.org",
    ),
    (
        "change middle of three (assoc)",
        r#"{
    "repositories": {
        "first": {
            "type": "package",
            "url": "https://first.test"
        },
        "foo": {
            "type": "vcs",
            "url": "https://old.example.org"
        },
        "bar": {
            "type": "vcs",
            "url": "https://other.example.org"
        }
    }
}
"#,
        r#"{
    "repositories": {
        "first": {
            "type": "package",
            "url": "https://first.test"
        },
        "foo": {
            "type": "vcs",
            "url": "https://new.example.org"
        },
        "bar": {
            "type": "vcs",
            "url": "https://other.example.org"
        }
    }
}
"#,
        "foo",
        "https://new.example.org",
    ),
    (
        "change last of three (assoc)",
        r#"{
    "repositories": {
        "first": {
            "type": "package",
            "url": "https://first.test"
        },
        "foo": {
            "type": "vcs",
            "url": "https://old.example.org"
        },
        "bar": {
            "type": "vcs",
            "url": "https://other.example.org"
        }
    }
}
"#,
        r#"{
    "repositories": {
        "first": {
            "type": "package",
            "url": "https://first.test"
        },
        "foo": {
            "type": "vcs",
            "url": "https://old.example.org"
        },
        "bar": {
            "type": "vcs",
            "url": "https://new.example.org"
        }
    }
}
"#,
        "bar",
        "https://new.example.org",
    ),
    (
        "change first of three (list)",
        r#"{
    "repositories": [
        {
            "name": "first",
            "type": "package",
            "url": "https://first.test"
        },
        {
            "name": "foo",
            "type": "vcs",
            "url": "https://old.example.org"
        },
        {
            "name": "bar",
            "type": "vcs",
            "url": "https://other.example.org"
        }
    ]
}
"#,
        r#"{
    "repositories": [
        {
            "name": "first",
            "type": "package",
            "url": "https://new.example.org"
        },
        {
            "name": "foo",
            "type": "vcs",
            "url": "https://old.example.org"
        },
        {
            "name": "bar",
            "type": "vcs",
            "url": "https://other.example.org"
        }
    ]
}
"#,
        "first",
        "https://new.example.org",
    ),
    (
        "change middle of three (list)",
        r#"{
    "repositories": [
        {
            "name": "first",
            "type": "package",
            "url": "https://first.test"
        },
        {
            "name": "foo",
            "type": "vcs",
            "url": "https://old.example.org"
        },
        {
            "name": "bar",
            "type": "vcs",
            "url": "https://other.example.org"
        }
    ]
}
"#,
        r#"{
    "repositories": [
        {
            "name": "first",
            "type": "package",
            "url": "https://first.test"
        },
        {
            "name": "foo",
            "type": "vcs",
            "url": "https://new.example.org"
        },
        {
            "name": "bar",
            "type": "vcs",
            "url": "https://other.example.org"
        }
    ]
}
"#,
        "foo",
        "https://new.example.org",
    ),
    (
        "change last of three (list)",
        r#"{
    "repositories": [
        {
            "name": "first",
            "type": "package",
            "url": "https://first.test"
        },
        {
            "name": "foo",
            "type": "vcs",
            "url": "https://old.example.org"
        },
        {
            "name": "bar",
            "type": "vcs",
            "url": "https://other.example.org"
        }
    ]
}
"#,
        r#"{
    "repositories": [
        {
            "name": "first",
            "type": "package",
            "url": "https://first.test"
        },
        {
            "name": "foo",
            "type": "vcs",
            "url": "https://old.example.org"
        },
        {
            "name": "bar",
            "type": "vcs",
            "url": "https://new.example.org"
        }
    ]
}
"#,
        "bar",
        "https://new.example.org",
    ),
];

const ADD_LIST_ITEM: &[(&str, &str, &str, &str, &str, bool)] = &[
    (
        "int-array first append to unset",
        "{}",
        r#"{
    "main": [1]
}
"#,
        "main",
        "1",
        true,
    ),
    (
        "int-array (one-line) first append to empty",
        r#"{
    "main": [ ]
}
"#,
        r#"{
    "main": [1]
}
"#,
        "main",
        "1",
        true,
    ),
    (
        "int-array (one-line) second append to non-empty-list",
        r#"{
    "main": [ 1 ]
}
"#,
        r#"{
    "main": [ 1, 2 ]
}
"#,
        "main",
        "2",
        true,
    ),
    (
        "object-array first append to unset",
        "{}",
        r#"{
    "main": [{
        "value": 1
    }]
}
"#,
        "main",
        r#"{"value":1}"#,
        true,
    ),
    (
        "object-array (one-line) first append to empty",
        r#"{
    "main": [ ]
}
"#,
        r#"{
    "main": [{
        "value": 2
    }]
}
"#,
        "main",
        r#"{"value":2}"#,
        true,
    ),
    (
        "mixed-array (one-line) second append to non-empty-list",
        r#"{
    "main": [ 1 ]
}
"#,
        r#"{
    "main": [ 1, {
        "value": 2
    } ]
}
"#,
        "main",
        r#"{"value":2}"#,
        true,
    ),
    (
        "int-array (multi-line) first append to empty",
        r#"{
    "main": [
    ]
}
"#,
        r#"{
    "main": [
        1
    ]
}
"#,
        "main",
        "1",
        true,
    ),
    (
        "int-array (multi-line) second append to non-empty-list",
        r#"{
    "main": [
        1
    ]
}
"#,
        r#"{
    "main": [
        1,
        2
    ]
}
"#,
        "main",
        "2",
        true,
    ),
    (
        "object-array (multi-line) first append to empty",
        r#"{
    "main": [
    ]
}
"#,
        r#"{
    "main": [
        {
            "value": 1
        }
    ]
}
"#,
        "main",
        r#"{"value":1}"#,
        true,
    ),
    (
        "mixed-array (multi-line) second append to non-empty-list",
        r#"{
    "main": [
        1
    ]
}
"#,
        r#"{
    "main": [
        1,
        {
            "value": 2
        }
    ]
}
"#,
        "main",
        r#"{"value":2}"#,
        true,
    ),
    (
        "int-array first prepend to unset",
        "{}",
        r#"{
    "main": [1]
}
"#,
        "main",
        "1",
        false,
    ),
    (
        "int-array (one-line) first prepend to empty",
        r#"{
    "main": [ ]
}
"#,
        r#"{
    "main": [1]
}
"#,
        "main",
        "1",
        false,
    ),
    (
        "int-array (one-line) second prepend to non-empty-list",
        r#"{
    "main": [ 1 ]
}
"#,
        r#"{
    "main": [ 2, 1 ]
}
"#,
        "main",
        "2",
        false,
    ),
    (
        "object-array first prepend to unset",
        "{}",
        r#"{
    "main": [{
        "value": 1
    }]
}
"#,
        "main",
        r#"{"value":1}"#,
        false,
    ),
    (
        "object-array (one-line) first prepend to empty",
        r#"{
    "main": [ ]
}
"#,
        r#"{
    "main": [{
        "value": 1
    }]
}
"#,
        "main",
        r#"{"value":1}"#,
        false,
    ),
    (
        "mixed-array (one-line) second prepend to non-empty-list",
        r#"{
    "main": [ 1 ]
}
"#,
        r#"{
    "main": [ {
        "value": 2
    }, 1 ]
}
"#,
        "main",
        r#"{"value":2}"#,
        false,
    ),
    (
        "int-array (multi-line) first prepend to empty",
        r#"{
    "main": [
    ]
}
"#,
        r#"{
    "main": [
        1
    ]
}
"#,
        "main",
        "1",
        false,
    ),
    (
        "int-array (multi-line) second prepend to non-empty-list",
        r#"{
    "main": [
        1
    ]
}
"#,
        r#"{
    "main": [
        2,
        1
    ]
}
"#,
        "main",
        "2",
        false,
    ),
    (
        "object-array (multi-line) first prepend to empty",
        r#"{
    "main": [
    ]
}
"#,
        r#"{
    "main": [
        {
            "value": 1
        }
    ]
}
"#,
        "main",
        r#"{"value":1}"#,
        false,
    ),
    (
        "mixed-array (multi-line) second prepend to non-empty-list",
        r#"{
    "main": [
        1
    ]
}
"#,
        r#"{
    "main": [
        {
            "value": 2
        },
        1
    ]
}
"#,
        "main",
        r#"{"value":2}"#,
        false,
    ),
];

const REMOVE_LIST_ITEM: &[(&str, &str, &str, &str, usize)] = &[
    (
        "int-array (one-line) remove first",
        r#"{
    "main": [
        1, 2, 3
    ]
}"#,
        r#"{
    "main": [
        2, 3
    ]
}
"#,
        "main",
        0,
    ),
    (
        "int-array (one-line) remove middle",
        r#"{
    "main": [
        1, 2, 3
    ]
}"#,
        r#"{
    "main": [
        1, 3
    ]
}
"#,
        "main",
        1,
    ),
    (
        "int-array (one-line) remove last",
        r#"{
    "main": [
        1, 2, 3
    ]
}"#,
        r#"{
    "main": [
        1, 2
    ]
}
"#,
        "main",
        2,
    ),
    (
        "int-array (multi-line) remove first",
        r#"{
    "main": [
        1,
        2,
        3
    ]
}"#,
        r#"{
    "main": [
        2,
        3
    ]
}
"#,
        "main",
        0,
    ),
    (
        "int-array (multi-line) remove middle",
        r#"{
    "main": [
        1,
        2,
        3
    ]
}"#,
        r#"{
    "main": [
        1,
        3
    ]
}
"#,
        "main",
        1,
    ),
    (
        "int-array (multi-line) remove last",
        r#"{
    "main": [
        1,
        2,
        3
    ]
}"#,
        r#"{
    "main": [
        1,
        2
    ]
}
"#,
        "main",
        2,
    ),
];

const INSERT_LIST_ITEM: &[(&str, &str, &str, &str, &str, usize)] = &[
    (
        "insert at 0 to unset list",
        r#"{
}
"#,
        r#"{
    "main": [{
        "foo": 1
    }]
}
"#,
        "main",
        r#"{"foo":1}"#,
        0,
    ),
    (
        "insert at 0 to empty list",
        r#"{
    "main": [
    ]
}
"#,
        r#"{
    "main": [
        {
            "foo": 1
        }
    ]
}
"#,
        "main",
        r#"{"foo":1}"#,
        0,
    ),
    (
        "insert at 0 list of 2",
        r#"{
    "main": [
        {
            "foo": 2
        },
        {
            "foo": 4
        }
    ]
}
"#,
        r#"{
    "main": [
        {
            "foo": 1
        },
        {
            "foo": 2
        },
        {
            "foo": 4
        }
    ]
}
"#,
        "main",
        r#"{"foo":1}"#,
        0,
    ),
    (
        "insert at 1 list of 2",
        r#"{
    "main": [
        {
            "foo": 2
        },
        {
            "foo": 4
        }
    ]
}
"#,
        r#"{
    "main": [
        {
            "foo": 2
        },
        {
            "foo": 3
        },
        {
            "foo": 4
        }
    ]
}
"#,
        "main",
        r#"{"foo":3}"#,
        1,
    ),
    (
        "insert at 2 list of 2",
        r#"{
    "main": [
        {
            "foo": 2
        },
        {
            "foo": 4
        }
    ]
}
"#,
        r#"{
    "main": [
        {
            "foo": 2
        },
        {
            "foo": 4
        },
        {
            "foo": 5
        }
    ]
}
"#,
        "main",
        r#"{"foo":5}"#,
        2,
    ),
];

#[test]
fn add_repository_can_initialize_empty_repositories() {
    let mut m = JsonManipulator::new(
        r#"{
  "repositories": {
  }
}"#,
    )
    .unwrap();
    assert!(m.add_repository("bar", &json!({"type": "composer"}), true));
    assert_eq!(
        m.contents(),
        r#"{
  "repositories": [
    {
      "name": "bar",
      "type": "composer"
    }
  ]
}
"#
    );
}

#[test]
fn add_repository_can_initialize_from_scratch() {
    let mut m = JsonManipulator::new("{\n\t\"a\": \"b\"\n}").unwrap();
    assert!(m.add_repository("bar2", &json!({"type": "composer"}), true));
    assert_eq!(m.contents(), "{\n\t\"a\": \"b\",\n\t\"repositories\": [{\n\t\t\"name\": \"bar2\",\n\t\t\"type\": \"composer\"\n\t}]\n}\n");
}

const FOO_REPOSITORY: &str = r#"{
    "repositories": [
        {
            "name": "foo",
            "type": "vcs",
            "url": "lala"
        }
    ]
}"#;

#[test]
fn add_repository_can_append() {
    let mut m = JsonManipulator::new(FOO_REPOSITORY).unwrap();
    assert!(m.add_repository("bar", &json!({"type": "composer"}), true));
    assert_eq!(
        m.contents(),
        r#"{
    "repositories": [
        {
            "name": "foo",
            "type": "vcs",
            "url": "lala"
        },
        {
            "name": "bar",
            "type": "composer"
        }
    ]
}
"#
    );
}

#[test]
fn add_repository_can_prepend() {
    let mut m = JsonManipulator::new(FOO_REPOSITORY).unwrap();
    assert!(m.add_repository("bar", &json!({"type": "composer"}), false));
    assert_eq!(
        m.contents(),
        r#"{
    "repositories": [
        {
            "name": "bar",
            "type": "composer"
        },
        {
            "name": "foo",
            "type": "vcs",
            "url": "lala"
        }
    ]
}
"#
    );
}

#[test]
fn add_repository() {
    check_cases(ADD_REPOSITORY, |&(label, from, to, name, config, append)| {
        let mut m = JsonManipulator::new(from).unwrap();
        if !m.add_repository(name, &serde_json::from_str(config).unwrap(), append) {
            return Err(format!("{label}: returned false"));
        }
        expect(&m, to).map_err(|e| format!("{label}: {e}"))
    });
}

#[test]
fn add_repository_can_override_deep_repos() {
    let mut m = JsonManipulator::new(
        r#"{
    "repositories": {
        "baz": {
            "type": "package",
            "package": {}
        }
    }
}"#,
    )
    .unwrap();
    assert!(m.add_repository("baz", &json!({"type": "composer"}), true));
    assert_eq!(
        m.contents(),
        r#"{
    "repositories": [
        {
            "name": "baz",
            "type": "composer"
        }
    ]
}
"#
    );
}

#[test]
fn set_url_in_repository() {
    check_cases(SET_REPOSITORY_URL, |&(label, from, to, name, url)| {
        let mut m = JsonManipulator::new(from).unwrap();
        if !m.set_repository_url(name, url) {
            return Err(format!("{label}: returned false"));
        }
        expect(&m, to).map_err(|e| format!("{label}: {e}"))
    });
}

#[test]
fn insert_repository_before_and_after_by_name() {
    let mut m = JsonManipulator::new(
        r#"{
    "repositories": {
        "alpha": {
            "type": "vcs",
            "url": "https://example.org/a"
        },
        "omega": {
            "type": "vcs",
            "url": "https://example.org/o"
        },
        "packagist.org": false
    }
}"#,
    )
    .unwrap();
    assert!(m.insert_repository("beta", &json!({"type": "vcs", "url": "https://example.org/b"}), "omega", 0));
    assert!(m.insert_repository("gamma", &json!({"type": "vcs", "url": "https://example.org/g"}), "alpha", 1));
    assert!(m.insert_repository("alpha", &json!({"type": "vcs", "url": "https://example.org/alpha"}), "gamma", 0));
    assert_eq!(
        m.contents(),
        r#"{
    "repositories": [
        {
            "name": "alpha",
            "type": "vcs",
            "url": "https://example.org/alpha"
        },
        {
            "name": "gamma",
            "type": "vcs",
            "url": "https://example.org/g"
        },
        {
            "name": "beta",
            "type": "vcs",
            "url": "https://example.org/b"
        },
        {
            "name": "omega",
            "type": "vcs",
            "url": "https://example.org/o"
        },
        {
            "packagist.org": false
        }
    ]
}
"#
    );
}

#[test]
fn remove_repository_removes_from_assoc_but_does_not_convert_from_assoc_to_list() {
    let mut m = JsonManipulator::new(
        r#"{
    "repositories": {
        "baz": {
            "type": "package",
            "package": {}
        },
        "packagist.org": false
    }
}"#,
    )
    .unwrap();
    assert!(m.remove_repository("baz"));
    assert_eq!(
        m.contents(),
        r#"{
    "repositories": {
        "packagist.org": false
    }
}
"#
    );
}

#[test]
fn remove_repository_removes_from_list() {
    let mut m = JsonManipulator::new(
        r#"{
    "repositories": [
        {
            "name": "baz",
            "type": "package",
            "package": {
            }
        },
        {
            "packagist.org": false
        }
    ]
}"#,
    )
    .unwrap();
    assert!(m.remove_repository("baz"));
    assert_eq!(
        m.contents(),
        r#"{
    "repositories": [
        {
            "packagist.org": false
        }
    ]
}
"#
    );
}

#[test]
fn add_repository_converts_from_assoc_to_list() {
    let mut m = JsonManipulator::new(
        r#"{
    "repositories": {
        "baz": {
            "type": "package",
            "package": {}
        },
        "packagist.org": false
    }
}"#,
    )
    .unwrap();
    assert!(m.add_repository("foo", &json!({"type": "composer"}), true));
    assert_eq!(
        m.contents(),
        r#"{
    "repositories": [
        {
            "name": "baz",
            "type": "package",
            "package": {
            }
        },
        {
            "packagist.org": false
        },
        {
            "name": "foo",
            "type": "composer"
        }
    ]
}
"#
    );
}

#[test]
fn add_config_setting_policy_list_field_does_surgical_edit() {
    let mut m = JsonManipulator::new(
        r#"{
    "name": "vendor/pkg",
    "config": {
        "vendor-dir": "vendor",
        "policy": {
            "advisories": {
                "audit": "report"
            },
            "abandoned": {
                "block": true
            }
        },
        "sort-packages": true
    }
}"#,
    )
    .unwrap();
    assert!(m.add_config_setting("policy.advisories.block", &json!(true)));
    assert_eq!(
        m.contents(),
        r#"{
    "name": "vendor/pkg",
    "config": {
        "vendor-dir": "vendor",
        "policy": {
            "advisories": {
                "audit": "report",
                "block": true
            },
            "abandoned": {
                "block": true
            }
        },
        "sort-packages": true
    }
}
"#
    );
}

#[test]
fn add_config_setting_policy_list_field_creates_missing_list() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
        "policy": {
        }
    }
}"#,
    )
    .unwrap();
    assert!(m.add_config_setting("policy.advisories.block", &json!(false)));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "policy": {
            "advisories": {
                "block": false
            }
        }
    }
}
"#
    );
}

#[test]
fn remove_config_setting_policy_list_field_does_surgical_edit() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
        "policy": {
            "advisories": {
                "audit": "report",
                "block": true
            }
        }
    }
}"#,
    )
    .unwrap();
    assert!(m.remove_config_setting("policy.advisories.audit"));
    assert_eq!(
        m.contents(),
        r#"{
    "config": {
        "policy": {
            "advisories": {
                "block": true
            }
        }
    }
}
"#
    );
}

#[test]
fn remove_config_setting_policy_list_field_no_op_when_absent() {
    let mut m = JsonManipulator::new(
        r#"{
    "config": {
        "policy": {
            "advisories": {
                "audit": "report"
            }
        }
    }
}"#,
    )
    .unwrap();
    assert!(m.remove_config_setting("policy.advisories.block"));
}

#[test]
fn add_list_item() {
    check_cases(ADD_LIST_ITEM, |&(label, from, to, main_node, value, append)| {
        let mut m = JsonManipulator::new(from).unwrap();
        if !m.add_list_item(main_node, &serde_json::from_str(value).unwrap(), append) {
            return Err(format!("{label}: returned false"));
        }
        expect(&m, to).map_err(|e| format!("{label}: {e}"))
    });
}

#[test]
fn remove_list_item() {
    check_cases(REMOVE_LIST_ITEM, |&(label, from, to, main_node, index)| {
        let mut m = JsonManipulator::new(from).unwrap();
        if !m.remove_list_item(main_node, index) {
            return Err(format!("{label}: returned false"));
        }
        expect(&m, to).map_err(|e| format!("{label}: {e}"))
    });
}

#[test]
fn insert_list_item() {
    check_cases(INSERT_LIST_ITEM, |&(label, from, to, main_node, value, index)| {
        let mut m = JsonManipulator::new(from).unwrap();
        if !m.insert_list_item(main_node, &serde_json::from_str(value).unwrap(), index) {
            return Err(format!("{label}: returned false"));
        }
        expect(&m, to).map_err(|e| format!("{label}: {e}"))
    });
}
