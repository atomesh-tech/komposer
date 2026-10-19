<?php

/*
 * komposer's implementation of the part of Composer's PHP API that script listeners use: Composer\Script\Event and
 * everything it hands out (the Composer instance, its config, IO, packages and repositories), so a `Class::method`
 * callback or a Command class runs without Composer. komposer starts it as
 *
 *     php script-runtime.php <payload.json> <result.json>
 *
 * The payload describes the event, the listener and the project; the result file gets the listener's return value,
 * whether it stopped the event, or the code of the exception it threw. Composer's own classes are only declared when
 * the project does not ship them, and the ones komposer cannot provide fail with an explanation.
 */

namespace Komposer\Script {
    /** Formats Symfony Console's `<info>`-style tags like Composer's output does. */
    final class Formatter
    {
        private const STYLES = [
            'error' => ['37', '41', []],
            'info' => ['32', null, []],
            'comment' => ['33', null, []],
            'question' => ['30', '46', []],
            'warning' => ['30', '43', []],
            'highlight' => ['31', null, []],
        ];
        private const COLORS = ['black' => 0, 'red' => 1, 'green' => 2, 'yellow' => 3, 'blue' => 4, 'magenta' => 5, 'cyan' => 6, 'white' => 7, 'default' => 9];
        private const OPTIONS = ['bold' => [1, 22], 'underscore' => [4, 24], 'blink' => [5, 25], 'reverse' => [7, 27], 'conceal' => [8, 28]];

        public static function format(string $message, bool $decorated): string
        {
            $out = '';
            $stack = [];
            $offset = 0;
            preg_match_all('#<(([a-z](?:[^\\\\<>]*)) | /([a-z](?:[^\\\\<>]*))?)>#ix', $message, $matches, PREG_OFFSET_CAPTURE);
            foreach ($matches[0] as $i => $match) {
                [$tag, $pos] = $match;
                if ($pos !== 0 && $message[$pos - 1] === '\\') {
                    continue;
                }
                $out .= self::apply(substr($message, $offset, $pos - $offset), end($stack) ?: null, $decorated);
                $offset = $pos + strlen($tag);
                if ($tag[1] === '/') {
                    array_pop($stack);
                    continue;
                }
                $style = self::style($matches[2][$i][0]);
                if ($style === null) {
                    $out .= self::apply($tag, end($stack) ?: null, $decorated);
                    continue;
                }
                $stack[] = $style;
            }
            $out .= self::apply(substr($message, $offset), end($stack) ?: null, $decorated);

            return strtr($out, ['\\<' => '<', '\\>' => '>']);
        }

        /** @return array{0: list<string>, 1: list<string>}|null */
        private static function style(string $name): ?array
        {
            if (isset(self::STYLES[strtolower($name)])) {
                [$fg, $bg, $options] = self::STYLES[strtolower($name)];
                return self::codes($fg, $bg, $options);
            }
            if (!preg_match_all('/([^=]+)=([^;]+)(;|$)/', $name, $parts, PREG_SET_ORDER)) {
                return null;
            }
            $fg = $bg = null;
            $options = [];
            foreach ($parts as [, $key, $value]) {
                $value = strtolower(trim($value));
                switch (strtolower(trim($key))) {
                    case 'fg':
                        $fg = isset(self::COLORS[$value]) ? '3'.self::COLORS[$value] : null;
                        break;
                    case 'bg':
                        $bg = isset(self::COLORS[$value]) ? '4'.self::COLORS[$value] : null;
                        break;
                    case 'options':
                        $options = array_map('trim', explode(',', $value));
                        break;
                    default:
                        return null;
                }
            }

            return self::codes($fg, $bg, $options);
        }

        private static function codes(?string $fg, ?string $bg, array $options): array
        {
            $set = $unset = [];
            if ($fg !== null) {
                $set[] = $fg;
                $unset[] = '39';
            }
            if ($bg !== null) {
                $set[] = $bg;
                $unset[] = '49';
            }
            foreach ($options as $option) {
                if (isset(self::OPTIONS[$option])) {
                    $set[] = (string) self::OPTIONS[$option][0];
                    $unset[] = (string) self::OPTIONS[$option][1];
                }
            }

            return [$set, $unset];
        }

        private static function apply(string $text, ?array $style, bool $decorated): string
        {
            if (!$decorated || $style === null || $text === '' || $style[0] === []) {
                return $text;
            }

            return sprintf("\033[%sm%s\033[%sm", implode(';', $style[0]), $text, implode(';', $style[1]));
        }
    }

    /** What komposer cannot provide without Composer itself. */
    final class Unsupported extends \LogicException
    {
        public static function method(string $method): self
        {
            return new self(sprintf('%s() is not available: komposer runs scripts without Composer itself, and does not provide it.', $method));
        }
    }

    /** A version constraint as Link::getConstraint() returns it, for its string forms. */
    final class Constraint
    {
        private $pretty;

        public function __construct(string $pretty)
        {
            $this->pretty = $pretty;
        }

        public function getPrettyString(): string
        {
            return $this->pretty;
        }

        public function __toString(): string
        {
            return $this->pretty;
        }
    }

    /** Symfony's QuestionHelper, for the ask*() and select() methods of ConsoleIO. */
    final class Prompt
    {
        public static function read(bool $hidden = false): ?string
        {
            $stty = $hidden && function_exists('shell_exec') && trim((string) @shell_exec('stty -g 2>/dev/null')) !== '';
            if ($stty) {
                $state = trim((string) shell_exec('stty -g'));
                shell_exec('stty -echo');
            }
            $line = fgets(STDIN, 4096);
            if ($stty) {
                shell_exec('stty '.escapeshellarg($state));
                fwrite(STDERR, PHP_EOL);
            }

            return $line === false ? null : rtrim($line, "\r\n");
        }
    }

    /** Symfony Application::renderThrowable(), as Composer prints an exception that ends a command. */
    final class Renderer
    {
        public static function render(\Throwable $e, \Composer\IO\ConsoleIO $io): void
        {
            $verbose = $io->isVerbose();
            $width = (int) (getenv('COLUMNS') ?: 0) ?: 80;
            $io->writeError('', true, \Composer\IO\IOInterface::QUIET);
            do {
                $message = trim($e->getMessage());
                $title = '';
                $len = 0;
                if ($message === '' || $verbose) {
                    $title = sprintf('  [%s%s]  ', get_class($e), 0 !== ($code = $e->getCode()) ? ' ('.$code.')' : '');
                    $len = self::width($title);
                }
                $lines = [];
                foreach ($message !== '' ? preg_split('/\r?\n/', $message) : [] as $line) {
                    foreach (self::split($line, $width - 1 - 4) as $chunk) {
                        $lines[] = [$chunk, self::width($chunk) + 4];
                        $len = max(self::width($chunk) + 4, $len);
                    }
                }
                $messages = [sprintf('<comment>%s</comment>', self::escape(sprintf('In %s line %s:', basename($e->getFile()) ?: 'n/a', $e->getLine() ?: 'n/a')))];
                $messages[] = $empty = sprintf('<error>%s</error>', str_repeat(' ', $len));
                if ($title !== '') {
                    $messages[] = sprintf('<error>%s%s</error>', $title, str_repeat(' ', max(0, $len - self::width($title))));
                }
                foreach ($lines as [$line, $lineLength]) {
                    $messages[] = sprintf('<error>  %s  %s</error>', self::escape($line), str_repeat(' ', $len - $lineLength));
                }
                $messages[] = $empty;
                $messages[] = '';
                $io->writeError($messages, true, \Composer\IO\IOInterface::QUIET);
                if ($verbose) {
                    $io->writeError('<comment>Exception trace:</comment>', true, \Composer\IO\IOInterface::QUIET);
                    $trace = $e->getTrace();
                    array_unshift($trace, ['function' => '', 'file' => $e->getFile() ?: 'n/a', 'line' => $e->getLine() ?: 'n/a']);
                    foreach ($trace as $frame) {
                        $function = $frame['function'] ?? '';
                        $io->writeError(sprintf(' %s%s at <info>%s:%s</info>', $frame['class'] ?? '', $function !== '' ? ($frame['type'] ?? '').$function.'()' : '', $frame['file'] ?? 'n/a', $frame['line'] ?? 'n/a'), true, \Composer\IO\IOInterface::QUIET);
                    }
                    $io->writeError('', true, \Composer\IO\IOInterface::QUIET);
                }
            } while ($e = $e->getPrevious());
        }

        private static function width(string $s): int
        {
            return function_exists('mb_strwidth') ? mb_strwidth($s, 'UTF-8') : strlen($s);
        }

        private static function split(string $s, int $width): array
        {
            if ($width <= 0 || $s === '') {
                return [$s];
            }

            return function_exists('mb_str_split') ? mb_str_split($s, $width, 'UTF-8') : str_split($s, $width);
        }

        private static function escape(string $s): string
        {
            return preg_replace('/([^\\\\]|^)([<>])/', '$1\\\\$2', $s);
        }
    }
}

namespace Composer\IO {
    if (!interface_exists(IOInterface::class)) {
        interface IOInterface
        {
            public const QUIET = 1;
            public const NORMAL = 2;
            public const VERBOSE = 4;
            public const VERY_VERBOSE = 8;
            public const DEBUG = 16;

            public function isInteractive();
            public function isVerbose();
            public function isVeryVerbose();
            public function isDebug();
            public function isDecorated();
            public function write($messages, bool $newline = true, int $verbosity = self::NORMAL);
            public function writeError($messages, bool $newline = true, int $verbosity = self::NORMAL);
            public function writeRaw($messages, bool $newline = true, int $verbosity = self::NORMAL);
            public function writeErrorRaw($messages, bool $newline = true, int $verbosity = self::NORMAL);
            public function overwrite($messages, bool $newline = true, ?int $size = null, int $verbosity = self::NORMAL);
            public function overwriteError($messages, bool $newline = true, ?int $size = null, int $verbosity = self::NORMAL);
            public function ask(string $question, $default = null);
            public function askConfirmation(string $question, bool $default = true);
            public function askAndValidate(string $question, callable $validator, ?int $attempts = null, $default = null);
            public function askAndHideAnswer(string $question);
            public function select(string $question, array $choices, $default, $attempts = false, string $errorMessage = 'Value "%s" is invalid', bool $multiselect = false);
            public function getAuthentications();
            public function hasAuthentication(string $repositoryName);
            public function getAuthentication(string $repositoryName);
            public function setAuthentication(string $repositoryName, string $username, ?string $password = null);
            public function loadConfiguration(\Composer\Config $config);
        }
    }

    if (!class_exists(BaseIO::class)) {
        abstract class BaseIO implements IOInterface
        {
            protected $authentications = [];

            public function getAuthentications()
            {
                return $this->authentications;
            }

            public function resetAuthentications()
            {
                $this->authentications = [];
            }

            public function hasAuthentication(string $repositoryName)
            {
                return isset($this->authentications[$repositoryName]);
            }

            public function getAuthentication(string $repositoryName)
            {
                return $this->authentications[$repositoryName] ?? ['username' => null, 'password' => null];
            }

            public function setAuthentication(string $repositoryName, string $username, ?string $password = null)
            {
                $this->authentications[$repositoryName] = ['username' => $username, 'password' => $password];
            }

            public function writeRaw($messages, bool $newline = true, int $verbosity = self::NORMAL)
            {
                $this->write($messages, $newline, $verbosity);
            }

            public function writeErrorRaw($messages, bool $newline = true, int $verbosity = self::NORMAL)
            {
                $this->writeError($messages, $newline, $verbosity);
            }

            public function loadConfiguration(\Composer\Config $config)
            {
            }

            public function emergency($message, array $context = []): void
            {
                $this->log('emergency', $message, $context);
            }

            public function alert($message, array $context = []): void
            {
                $this->log('alert', $message, $context);
            }

            public function critical($message, array $context = []): void
            {
                $this->log('critical', $message, $context);
            }

            public function error($message, array $context = []): void
            {
                $this->log('error', $message, $context);
            }

            public function warning($message, array $context = []): void
            {
                $this->log('warning', $message, $context);
            }

            public function notice($message, array $context = []): void
            {
                $this->log('notice', $message, $context);
            }

            public function info($message, array $context = []): void
            {
                $this->log('info', $message, $context);
            }

            public function debug($message, array $context = []): void
            {
                $this->log('debug', $message, $context);
            }

            public function log($level, $message, array $context = []): void
            {
                $message = (string) $message;
                if ($context !== []) {
                    $json = json_encode($context, JSON_INVALID_UTF8_IGNORE | JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
                    if ($json !== false) {
                        $message .= ' ' . $json;
                    }
                }
                if (in_array($level, ['emergency', 'alert', 'critical', 'error'], true)) {
                    $this->writeError('<error>'.$message.'</error>');
                } elseif ($level === 'warning') {
                    $this->writeError('<warning>'.$message.'</warning>');
                } elseif ($level === 'notice') {
                    $this->writeError('<info>'.$message.'</info>', true, self::VERBOSE);
                } elseif ($level === 'info') {
                    $this->writeError('<info>'.$message.'</info>', true, self::VERY_VERBOSE);
                } else {
                    $this->writeError($message, true, self::DEBUG);
                }
            }
        }
    }

    if (!class_exists(ConsoleIO::class)) {
        /** Composer's ConsoleIO over komposer's terminal: stdout and stderr with komposer's verbosity and colors. */
        class ConsoleIO extends BaseIO
        {
            private $verbosity;
            private $decorated;
            private $interactive;
            private $lastMessage = '';
            private $lastMessageErr = '';

            public function __construct(int $verbosity = self::NORMAL, bool $decorated = false, bool $interactive = false)
            {
                $this->verbosity = $verbosity;
                $this->decorated = $decorated;
                $this->interactive = $interactive;
            }

            public function isInteractive()
            {
                return $this->interactive;
            }

            public function isDecorated()
            {
                return $this->decorated;
            }

            public function isVerbose()
            {
                return $this->verbosity >= self::VERBOSE;
            }

            public function isVeryVerbose()
            {
                return $this->verbosity >= self::VERY_VERBOSE;
            }

            public function isDebug()
            {
                return $this->verbosity >= self::DEBUG;
            }

            public function write($messages, bool $newline = true, int $verbosity = self::NORMAL)
            {
                $this->doWrite($messages, $newline, false, $verbosity, false);
            }

            public function writeError($messages, bool $newline = true, int $verbosity = self::NORMAL)
            {
                $this->doWrite($messages, $newline, true, $verbosity, false);
            }

            public function writeRaw($messages, bool $newline = true, int $verbosity = self::NORMAL)
            {
                $this->doWrite($messages, $newline, false, $verbosity, true);
            }

            public function writeErrorRaw($messages, bool $newline = true, int $verbosity = self::NORMAL)
            {
                $this->doWrite($messages, $newline, true, $verbosity, true);
            }

            private function doWrite($messages, bool $newline, bool $stderr, int $verbosity, bool $raw): void
            {
                if ($verbosity > $this->verbosity) {
                    return;
                }
                $out = '';
                foreach ((array) $messages as $message) {
                    $message = (string) $message;
                    $out .= ($raw ? $message : \Komposer\Script\Formatter::format($message, $this->decorated)).($newline ? PHP_EOL : '');
                }
                fwrite($stderr ? STDERR : STDOUT, $out);
                if ($stderr) {
                    $this->lastMessageErr = implode($newline ? "\n" : '', (array) $messages);
                } else {
                    $this->lastMessage = implode($newline ? "\n" : '', (array) $messages);
                }
            }

            public function overwrite($messages, bool $newline = true, ?int $size = null, int $verbosity = self::NORMAL)
            {
                $this->doOverwrite($messages, $newline, $size, false, $verbosity);
            }

            public function overwriteError($messages, bool $newline = true, ?int $size = null, int $verbosity = self::NORMAL)
            {
                $this->doOverwrite($messages, $newline, $size, true, $verbosity);
            }

            private function doOverwrite($messages, bool $newline, ?int $size, bool $stderr, int $verbosity): void
            {
                $messages = implode($newline ? "\n" : '', (array) $messages);
                if (!isset($size)) {
                    $size = strlen(strip_tags($stderr ? $this->lastMessageErr : $this->lastMessage));
                }
                $this->doWrite(str_repeat("\x08", $size), false, $stderr, $verbosity, false);
                $this->doWrite($messages, false, $stderr, $verbosity, false);
                $fill = $size - strlen(strip_tags($messages));
                if ($fill > 0) {
                    $this->doWrite(str_repeat(' ', $fill), false, $stderr, $verbosity, false);
                    $this->doWrite(str_repeat("\x08", $fill), false, $stderr, $verbosity, false);
                }
                if ($newline) {
                    $this->doWrite('', true, $stderr, $verbosity, false);
                }
                if ($stderr) {
                    $this->lastMessageErr = $messages;
                } else {
                    $this->lastMessage = $messages;
                }
            }

            private function prompt(string $question): void
            {
                fwrite(STDERR, \Komposer\Script\Formatter::format($question, $this->decorated));
            }

            public function ask(string $question, $default = null)
            {
                if (!$this->interactive) {
                    return $default;
                }
                $this->prompt($question);
                $answer = \Komposer\Script\Prompt::read();

                return $answer === null || $answer === '' ? $default : $answer;
            }

            public function askConfirmation(string $question, bool $default = true)
            {
                if (!$this->interactive) {
                    return $default;
                }
                $this->prompt($question);
                $answer = \Komposer\Script\Prompt::read();
                if ($answer === null || $answer === '') {
                    return $default;
                }

                return (bool) preg_match('/^y/i', $answer);
            }

            public function askAndValidate(string $question, callable $validator, ?int $attempts = null, $default = null)
            {
                $error = null;
                for ($attempt = 0; $attempts === null || $attempt < $attempts; $attempt++) {
                    if ($error !== null) {
                        $this->writeError('<error>'.$error->getMessage().'</error>');
                    }
                    try {
                        return $validator($this->ask($question, $default));
                    } catch (\Exception $e) {
                        $error = $e;
                        if (!$this->interactive) {
                            throw $e;
                        }
                    }
                }
                throw $error;
            }

            public function askAndHideAnswer(string $question)
            {
                if (!$this->interactive) {
                    return null;
                }
                $this->prompt($question);

                return \Komposer\Script\Prompt::read(true);
            }

            public function select(string $question, array $choices, $default, $attempts = false, string $errorMessage = 'Value "%s" is invalid', bool $multiselect = false)
            {
                $width = max(array_map('strlen', array_map('strval', array_keys($choices))) ?: [0]);
                $lines = [$question];
                foreach ($choices as $key => $value) {
                    $lines[] = sprintf('  [<comment>%s</comment>] %s', str_pad((string) $key, $width), $value);
                }
                $validate = function ($answer) use ($choices, $errorMessage, $multiselect) {
                    $answer = (string) $answer;
                    $values = $multiselect ? array_map('trim', explode(',', $answer)) : [trim($answer)];
                    $keys = [];
                    foreach ($values as $value) {
                        $key = array_key_exists($value, $choices) ? $value : array_search($value, $choices, true);
                        if ($key === false) {
                            throw new \InvalidArgumentException(sprintf($errorMessage, $value));
                        }
                        $keys[] = (string) $key;
                    }

                    return $multiselect ? $keys : $keys[0];
                };
                $this->writeError($lines);

                return $this->askAndValidate('> ', $validate, $attempts === false ? null : (int) $attempts, $default);
            }
        }
    }

    if (!class_exists(NullIO::class)) {
        class NullIO extends ConsoleIO
        {
            public function __construct()
            {
                parent::__construct(self::QUIET, false, false);
            }

            public function write($messages, bool $newline = true, int $verbosity = self::NORMAL)
            {
            }

            public function writeError($messages, bool $newline = true, int $verbosity = self::NORMAL)
            {
            }
        }
    }
}

namespace Composer\EventDispatcher {
    if (!class_exists(Event::class)) {
        class Event
        {
            protected $name;
            protected $args;
            protected $flags;
            private $propagationStopped = false;

            public function __construct(string $name, array $args = [], array $flags = [])
            {
                $this->name = $name;
                $this->args = $args;
                $this->flags = $flags;
            }

            public function getName()
            {
                return $this->name;
            }

            public function getArguments()
            {
                return $this->args;
            }

            public function getFlags()
            {
                return $this->flags;
            }

            public function isPropagationStopped()
            {
                return $this->propagationStopped;
            }

            public function stopPropagation()
            {
                $this->propagationStopped = true;
            }
        }
    }

    if (!class_exists(EventDispatcher::class)) {
        /** Other scripts run through komposer itself. */
        class EventDispatcher
        {
            public function dispatchScript(string $eventName, bool $devMode = false, array $additionalArgs = [], array $flags = [])
            {
                if (!$this->hasEventListeners(new Event($eventName))) {
                    return 0;
                }
                $command = escapeshellarg((string) getenv('COMPOSER_BINARY')).' run-script '.($devMode ? '--dev ' : '--no-dev ').escapeshellarg($eventName);
                foreach ($additionalArgs as $arg) {
                    $command .= ' '.escapeshellarg((string) $arg);
                }
                passthru($command, $code);

                return $code;
            }

            public function hasEventListeners(Event $event)
            {
                $scripts = $GLOBALS['komposer_payload']['root']['scripts'] ?? [];

                return !empty($scripts[$event->getName()]);
            }

            public function __call($name, $arguments)
            {
                throw \Komposer\Script\Unsupported::method('EventDispatcher::'.$name);
            }
        }
    }
}

namespace Composer\Script {
    if (!class_exists(Event::class)) {
        class Event extends \Composer\EventDispatcher\Event
        {
            private $composer;
            private $io;
            private $devMode;
            private $originatingEvent;

            public function __construct(string $name, \Composer\Composer $composer, \Composer\IO\IOInterface $io, bool $devMode = false, array $args = [], array $flags = [])
            {
                parent::__construct($name, $args, $flags);
                $this->composer = $composer;
                $this->io = $io;
                $this->devMode = $devMode;
            }

            public function getComposer()
            {
                return $this->composer;
            }

            public function getIO()
            {
                return $this->io;
            }

            public function isDevMode()
            {
                return $this->devMode;
            }

            public function getOriginatingEvent()
            {
                return $this->originatingEvent;
            }

            public function setOriginatingEvent(\Composer\EventDispatcher\Event $event)
            {
                $this->originatingEvent = $event;

                return $this;
            }
        }
    }

    if (!class_exists(ScriptEvents::class)) {
        class ScriptEvents
        {
            public const PRE_INSTALL_CMD = 'pre-install-cmd';
            public const POST_INSTALL_CMD = 'post-install-cmd';
            public const PRE_UPDATE_CMD = 'pre-update-cmd';
            public const POST_UPDATE_CMD = 'post-update-cmd';
            public const PRE_STATUS_CMD = 'pre-status-cmd';
            public const POST_STATUS_CMD = 'post-status-cmd';
            public const PRE_AUTOLOAD_DUMP = 'pre-autoload-dump';
            public const POST_AUTOLOAD_DUMP = 'post-autoload-dump';
            public const POST_ROOT_PACKAGE_INSTALL = 'post-root-package-install';
            public const POST_CREATE_PROJECT_CMD = 'post-create-project-cmd';
            public const PRE_ARCHIVE_CMD = 'pre-archive-cmd';
            public const POST_ARCHIVE_CMD = 'post-archive-cmd';
        }
    }
}

namespace Composer\DependencyResolver\Operation {
    if (!interface_exists(OperationInterface::class)) {
        interface OperationInterface
        {
            public function getOperationType();
            public function show($lock);
            public function __toString();
        }
    }

    if (!class_exists(SolverOperation::class)) {
        abstract class SolverOperation implements OperationInterface
        {
            public const TYPE = '';

            public function getOperationType()
            {
                return static::TYPE;
            }

            public function __toString()
            {
                return $this->show(false);
            }

            protected static function format(\Composer\Package\PackageInterface $package, bool $lock): string
            {
                return '<info>'.$package->getPrettyName().'</info> (<comment>'.($lock ? $package->getPrettyVersion() : $package->getFullPrettyVersion()).'</comment>)';
            }
        }
    }

    if (!class_exists(InstallOperation::class)) {
        class InstallOperation extends SolverOperation
        {
            public const TYPE = 'install';
            protected $package;

            public function __construct(\Composer\Package\PackageInterface $package)
            {
                $this->package = $package;
            }

            public function getPackage()
            {
                return $this->package;
            }

            public function show($lock)
            {
                return ($lock ? 'Locking ' : 'Installing ').self::format($this->package, $lock);
            }
        }
    }

    if (!class_exists(UninstallOperation::class)) {
        class UninstallOperation extends SolverOperation
        {
            public const TYPE = 'uninstall';
            protected $package;

            public function __construct(\Composer\Package\PackageInterface $package)
            {
                $this->package = $package;
            }

            public function getPackage()
            {
                return $this->package;
            }

            public function show($lock)
            {
                return 'Removing '.self::format($this->package, $lock);
            }
        }
    }

    if (!class_exists(UpdateOperation::class)) {
        class UpdateOperation extends SolverOperation
        {
            public const TYPE = 'update';
            protected $initialPackage;
            protected $targetPackage;

            public function __construct(\Composer\Package\PackageInterface $initial, \Composer\Package\PackageInterface $target)
            {
                $this->initialPackage = $initial;
                $this->targetPackage = $target;
            }

            public function getInitialPackage()
            {
                return $this->initialPackage;
            }

            public function getTargetPackage()
            {
                return $this->targetPackage;
            }

            public function show($lock)
            {
                $from = $this->initialPackage->getFullPrettyVersion();
                $to = $this->targetPackage->getFullPrettyVersion();
                $action = version_compare($this->targetPackage->getVersion(), $this->initialPackage->getVersion(), '<') ? 'Downgrading' : 'Upgrading';

                return ($lock ? 'Locking' : $action).' <info>'.$this->initialPackage->getPrettyName().'</info> (<comment>'.$from.'</comment> => <comment>'.$to.'</comment>)';
            }
        }
    }
}

namespace Composer\DependencyResolver {
    if (!class_exists(Transaction::class)) {
        class Transaction
        {
            protected $operations;

            public function __construct(array $operations)
            {
                $this->operations = $operations;
            }

            public function getOperations()
            {
                return $this->operations;
            }
        }
    }
}

namespace Composer\Package {
    if (!interface_exists(PackageInterface::class)) {
        interface PackageInterface
        {
            public const DISPLAY_SOURCE_REF_IF_DEV = 0;
            public const DISPLAY_SOURCE_REF = 1;
            public const DISPLAY_DIST_REF = 2;

            public function getName();
            public function getPrettyName();
            public function getNames($provides = true);
            public function getType();
            public function getTargetDir();
            public function getExtra();
            public function getInstallationSource();
            public function getSourceType();
            public function getSourceUrl();
            public function getSourceReference();
            public function getDistType();
            public function getDistUrl();
            public function getDistReference();
            public function getDistSha1Checksum();
            public function getVersion();
            public function getPrettyVersion();
            public function getFullPrettyVersion(bool $truncate = true, int $displayMode = self::DISPLAY_SOURCE_REF_IF_DEV);
            public function getReleaseDate();
            public function getStability();
            public function getRequires();
            public function getConflicts();
            public function getProvides();
            public function getReplaces();
            public function getDevRequires();
            public function getSuggests();
            public function getAutoload();
            public function getDevAutoload();
            public function getIncludePaths();
            public function getBinaries();
            public function getUniqueName();
            public function getNotificationUrl();
            public function isDev();
            public function getPrettyString();
            public function __toString();
        }
    }

    if (!interface_exists(CompletePackageInterface::class)) {
        interface CompletePackageInterface extends PackageInterface
        {
            public function getScripts();
            public function getRepositories();
            public function getLicense();
            public function getKeywords();
            public function getDescription();
            public function getHomepage();
            public function getAuthors();
            public function getSupport();
            public function getFunding();
            public function isAbandoned();
            public function getReplacementPackage();
        }
    }

    if (!interface_exists(RootPackageInterface::class)) {
        interface RootPackageInterface extends CompletePackageInterface
        {
            public function getAliases();
            public function getMinimumStability();
            public function getStabilityFlags();
            public function getReferences();
            public function getPreferStable();
            public function getConfig();
        }
    }

    if (!class_exists(Link::class)) {
        class Link
        {
            protected $source;
            protected $target;
            protected $constraint;
            protected $description;
            protected $prettyConstraint;

            public function __construct(string $source, string $target, $constraint, string $description = 'relates to', ?string $prettyConstraint = null)
            {
                $this->source = strtolower($source);
                $this->target = strtolower($target);
                $this->constraint = $constraint;
                $this->description = $description;
                $this->prettyConstraint = $prettyConstraint;
            }

            public function getDescription()
            {
                return $this->description;
            }

            public function getSource()
            {
                return $this->source;
            }

            public function getTarget()
            {
                return $this->target;
            }

            public function getConstraint()
            {
                return $this->constraint;
            }

            public function getPrettyConstraint()
            {
                return $this->prettyConstraint ?? (string) $this->constraint;
            }

            public function __toString()
            {
                return $this->source.' '.$this->description.' '.$this->target.' ('.$this->constraint.')';
            }

            public function getPrettyString(PackageInterface $sourcePackage)
            {
                return $sourcePackage->getPrettyString().' '.$this->description.' '.$this->target.' '.$this->getPrettyConstraint();
            }
        }
    }

    if (!class_exists(CompletePackage::class)) {
        /** A package from its installed.json / composer.lock array. */
        class CompletePackage implements CompletePackageInterface
        {
            protected $data;
            protected $name;
            protected $version;

            public function __construct(array $data)
            {
                $this->data = $data;
                $this->name = strtolower($data['name']);
                $this->version = $data['version_normalized'] ?? $data['version'] ?? '';
            }

            protected function get(string $key, $default = null)
            {
                return $this->data[$key] ?? $default;
            }

            protected function links(string $key, string $description): array
            {
                $links = [];
                foreach ($this->get($key, []) as $target => $constraint) {
                    $links[strtolower($target)] = new Link($this->name, $target, new \Komposer\Script\Constraint((string) $constraint), $description, (string) $constraint);
                }

                return $links;
            }

            public function getName()
            {
                return $this->name;
            }

            public function getPrettyName()
            {
                return $this->data['name'];
            }

            public function getNames($provides = true)
            {
                $names = [$this->name => $this->name];
                if ($provides) {
                    foreach ($this->getProvides() as $link) {
                        $names[$link->getTarget()] = $link->getTarget();
                    }
                }
                foreach ($this->getReplaces() as $link) {
                    $names[$link->getTarget()] = $link->getTarget();
                }

                return array_keys($names);
            }

            public function getType()
            {
                return $this->get('type', 'library');
            }

            public function getTargetDir()
            {
                $dir = $this->get('target-dir');

                return $dir === null ? null : ltrim(preg_replace('{ (?:^|[\\\\/]+) \.\.? (?:[\\\\/]+|$) (?:\.\.? (?:[\\\\/]+|$) )*}x', '/', $dir), '/');
            }

            public function getExtra()
            {
                return $this->get('extra', []);
            }

            public function getInstallationSource()
            {
                return $this->get('installation-source');
            }

            public function getSourceType()
            {
                return $this->data['source']['type'] ?? null;
            }

            public function getSourceUrl()
            {
                return $this->data['source']['url'] ?? null;
            }

            public function getSourceUrls()
            {
                return array_filter([$this->getSourceUrl()]);
            }

            public function getSourceReference()
            {
                return $this->data['source']['reference'] ?? null;
            }

            public function getSourceMirrors()
            {
                return $this->data['source']['mirrors'] ?? null;
            }

            public function getDistType()
            {
                return $this->data['dist']['type'] ?? null;
            }

            public function getDistUrl()
            {
                return $this->data['dist']['url'] ?? null;
            }

            public function getDistUrls()
            {
                return array_filter([$this->getDistUrl()]);
            }

            public function getDistReference()
            {
                return $this->data['dist']['reference'] ?? null;
            }

            public function getDistSha1Checksum()
            {
                return $this->data['dist']['shasum'] ?? null;
            }

            public function getDistMirrors()
            {
                return $this->data['dist']['mirrors'] ?? null;
            }

            public function getTransportOptions()
            {
                return $this->get('transport-options', []);
            }

            public function getVersion()
            {
                return $this->version;
            }

            public function getPrettyVersion()
            {
                return $this->data['version'] ?? $this->version;
            }

            public function getFullPrettyVersion(bool $truncate = true, int $displayMode = PackageInterface::DISPLAY_SOURCE_REF_IF_DEV)
            {
                if ($displayMode === PackageInterface::DISPLAY_SOURCE_REF_IF_DEV && (!$this->isDev() || !in_array($this->getSourceType(), ['hg', 'git'], true))) {
                    return $this->getPrettyVersion();
                }
                $reference = $displayMode === PackageInterface::DISPLAY_DIST_REF ? $this->getDistReference() : $this->getSourceReference();
                if ($reference === null) {
                    return $this->getPrettyVersion();
                }
                if ($truncate && strlen($reference) === 40 && $this->getSourceType() !== 'svn') {
                    return $this->getPrettyVersion().' '.substr($reference, 0, 7);
                }

                return $this->getPrettyVersion().' '.$reference;
            }

            public function getReleaseDate()
            {
                $time = $this->get('time');

                return $time === null ? null : new \DateTime($time, new \DateTimeZone('UTC'));
            }

            /** `VersionParser::parseStability()` of the normalized version. */
            public function getStability()
            {
                $version = (string) preg_replace('{#.+$}', '', $this->version);
                if (strpos($version, 'dev-') === 0 || '-dev' === substr($version, -4)) {
                    return 'dev';
                }
                preg_match('{[._-]?(?:(stable|beta|b|RC|alpha|a|patch|pl|p)((?:[.-]?\d+)*+)?)?([.-]?dev)?(?:\+.*)?$}i', strtolower($version), $match);
                if (!empty($match[3])) {
                    return 'dev';
                }
                if (!empty($match[1])) {
                    if ('beta' === $match[1] || 'b' === $match[1]) {
                        return 'beta';
                    }
                    if ('alpha' === $match[1] || 'a' === $match[1]) {
                        return 'alpha';
                    }
                    if ('rc' === $match[1]) {
                        return 'RC';
                    }
                }

                return 'stable';
            }

            public function isDev()
            {
                return $this->getStability() === 'dev';
            }

            public function getRequires()
            {
                return $this->links('require', 'requires');
            }

            public function getDevRequires()
            {
                return $this->links('require-dev', 'requires (for development)');
            }

            public function getConflicts()
            {
                return $this->links('conflict', 'conflicts');
            }

            public function getProvides()
            {
                return $this->links('provide', 'provides');
            }

            public function getReplaces()
            {
                return $this->links('replace', 'replaces');
            }

            public function getSuggests()
            {
                return $this->get('suggest', []);
            }

            public function getAutoload()
            {
                return $this->get('autoload', []);
            }

            public function getDevAutoload()
            {
                return $this->get('autoload-dev', []);
            }

            public function getIncludePaths()
            {
                return $this->get('include-path', []);
            }

            public function getBinaries()
            {
                return $this->get('bin', []);
            }

            public function getNotificationUrl()
            {
                return $this->get('notification-url');
            }

            public function getScripts()
            {
                return $this->get('scripts', []);
            }

            public function getRepositories()
            {
                return $this->get('repositories', []);
            }

            public function getLicense()
            {
                return (array) $this->get('license', []);
            }

            public function getKeywords()
            {
                return $this->get('keywords', []);
            }

            public function getDescription()
            {
                return $this->get('description');
            }

            public function getHomepage()
            {
                return $this->get('homepage');
            }

            public function getAuthors()
            {
                return $this->get('authors', []);
            }

            public function getSupport()
            {
                return $this->get('support', []);
            }

            public function getFunding()
            {
                return $this->get('funding', []);
            }

            public function getArchiveName()
            {
                return $this->data['archive']['name'] ?? null;
            }

            public function getArchiveExcludes()
            {
                return $this->data['archive']['exclude'] ?? [];
            }

            public function isAbandoned()
            {
                return (bool) $this->get('abandoned', false);
            }

            public function getReplacementPackage()
            {
                $abandoned = $this->get('abandoned', false);

                return is_string($abandoned) ? $abandoned : null;
            }

            public function isDefaultBranch()
            {
                return (bool) $this->get('default-branch', false);
            }

            public function getUniqueName()
            {
                return $this->name.'-'.$this->version;
            }

            public function getPrettyString()
            {
                return $this->getPrettyName().' '.$this->getPrettyVersion();
            }

            public function getRepository()
            {
                return null;
            }

            public function __toString()
            {
                return $this->getUniqueName();
            }
        }
    }

    if (!class_exists(Package::class)) {
        class Package extends CompletePackage
        {
        }
    }

    if (!class_exists(RootPackage::class)) {
        /** composer.json, with the version komposer resolved for it. */
        class RootPackage extends CompletePackage implements RootPackageInterface
        {
            public function getAliases()
            {
                return $this->get('aliases', []);
            }

            public function getMinimumStability()
            {
                return $this->get('minimum-stability', 'stable');
            }

            public function getStabilityFlags()
            {
                return $this->get('stability-flags', []);
            }

            public function getReferences()
            {
                return $this->get('references', []);
            }

            public function getPreferStable()
            {
                return (bool) $this->get('prefer-stable', false);
            }

            public function getConfig()
            {
                return $this->get('config', []);
            }

            public function setScripts(array $scripts)
            {
                $this->data['scripts'] = $scripts;
            }

            public function setExtra(array $extra)
            {
                $this->data['extra'] = $extra;
            }

            public function __call($name, $arguments)
            {
                throw \Komposer\Script\Unsupported::method('RootPackage::'.$name);
            }
        }
    }
}

namespace Composer\Repository {
    if (!class_exists(InstalledArrayRepository::class)) {
        /** vendor/composer/installed.json. */
        class InstalledArrayRepository implements \Countable
        {
            protected $packages;
            protected $devMode;
            protected $devPackageNames;

            public function __construct(array $packages, ?bool $devMode, array $devPackageNames)
            {
                $this->packages = $packages;
                $this->devMode = $devMode;
                $this->devPackageNames = $devPackageNames;
            }

            public function getRepoName()
            {
                return 'installed repo';
            }

            public function getPackages()
            {
                return $this->packages;
            }

            public function getCanonicalPackages()
            {
                $canonical = [];
                foreach ($this->packages as $package) {
                    $canonical[$package->getName()] = $canonical[$package->getName()] ?? $package;
                }

                return array_values($canonical);
            }

            public function getDevMode()
            {
                return $this->devMode;
            }

            public function getDevPackageNames()
            {
                return $this->devPackageNames;
            }

            public function hasPackage(\Composer\Package\PackageInterface $package)
            {
                foreach ($this->packages as $candidate) {
                    if ($candidate->getUniqueName() === $package->getUniqueName()) {
                        return true;
                    }
                }

                return false;
            }

            public function findPackage(string $name, $constraint)
            {
                return $this->findPackages($name, $constraint)[0] ?? null;
            }

            public function findPackages(string $name, $constraint = null)
            {
                $name = strtolower($name);
                $found = [];
                foreach ($this->packages as $package) {
                    if ($package->getName() === $name && self::matches($package, $constraint)) {
                        $found[] = $package;
                    }
                }

                return $found;
            }

            private static function matches(\Composer\Package\PackageInterface $package, $constraint): bool
            {
                $constraint = $constraint === null ? '*' : (string) $constraint;
                if ($constraint === '*' || $constraint === '') {
                    return true;
                }
                if ($constraint === $package->getVersion() || $constraint === $package->getPrettyVersion()) {
                    return true;
                }
                if (class_exists('Composer\Semver\VersionParser') && class_exists('Composer\Semver\Constraint\Constraint')) {
                    $parser = new \Composer\Semver\VersionParser();

                    return $parser->parseConstraints($constraint)->matches(new \Composer\Semver\Constraint\Constraint('==', $package->getVersion()));
                }
                throw new \LogicException('Matching packages against "'.$constraint.'" needs composer/semver installed in the project when komposer runs scripts.');
            }

            public function search(string $query, int $mode = 0, ?string $type = null)
            {
                throw \Komposer\Script\Unsupported::method('InstalledRepository::search');
            }

            public function count(): int
            {
                return count($this->packages);
            }
        }
    }

    if (!class_exists(InstalledFilesystemRepository::class)) {
        class InstalledFilesystemRepository extends InstalledArrayRepository
        {
        }
    }

    if (!class_exists(RepositoryManager::class)) {
        class RepositoryManager
        {
            private $localRepository;

            public function __construct(InstalledFilesystemRepository $localRepository)
            {
                $this->localRepository = $localRepository;
            }

            public function getLocalRepository()
            {
                return $this->localRepository;
            }

            public function getRepositories()
            {
                return [];
            }

            public function __call($name, $arguments)
            {
                throw \Komposer\Script\Unsupported::method('RepositoryManager::'.$name);
            }
        }
    }
}

namespace Composer\Installer {
    if (!class_exists(InstallationManager::class)) {
        class InstallationManager
        {
            private $vendorDir;

            public function __construct(string $vendorDir)
            {
                $this->vendorDir = $vendorDir;
            }

            /** `vendor-dir/name[/target-dir]`; metapackages have none. */
            public function getInstallPath(\Composer\Package\PackageInterface $package)
            {
                if ($package->getType() === 'metapackage') {
                    return null;
                }
                $path = $this->vendorDir.'/'.$package->getPrettyName();

                return $package->getTargetDir() ? $path.'/'.$package->getTargetDir() : $path;
            }

            public function isPackageInstalled($repo, \Composer\Package\PackageInterface $package)
            {
                return $repo->hasPackage($package) && is_dir((string) $this->getInstallPath($package));
            }

            public function __call($name, $arguments)
            {
                throw \Komposer\Script\Unsupported::method('InstallationManager::'.$name);
            }
        }
    }

    if (!class_exists(PackageEvent::class)) {
        class PackageEvent extends \Composer\EventDispatcher\Event
        {
            private $composer;
            private $io;
            private $devMode;
            private $localRepo;
            private $operations;
            private $operation;

            public function __construct(string $eventName, \Composer\Composer $composer, \Composer\IO\IOInterface $io, bool $devMode, $localRepo, array $operations, \Composer\DependencyResolver\Operation\OperationInterface $operation)
            {
                parent::__construct($eventName);
                $this->composer = $composer;
                $this->io = $io;
                $this->devMode = $devMode;
                $this->localRepo = $localRepo;
                $this->operations = $operations;
                $this->operation = $operation;
            }

            public function getComposer()
            {
                return $this->composer;
            }

            public function getIO()
            {
                return $this->io;
            }

            public function isDevMode()
            {
                return $this->devMode;
            }

            public function getLocalRepo()
            {
                return $this->localRepo;
            }

            public function getOperations()
            {
                return $this->operations;
            }

            public function getOperation()
            {
                return $this->operation;
            }
        }
    }

    if (!class_exists(PackageEvents::class)) {
        class PackageEvents
        {
            public const PRE_PACKAGE_INSTALL = 'pre-package-install';
            public const POST_PACKAGE_INSTALL = 'post-package-install';
            public const PRE_PACKAGE_UPDATE = 'pre-package-update';
            public const POST_PACKAGE_UPDATE = 'post-package-update';
            public const PRE_PACKAGE_UNINSTALL = 'pre-package-uninstall';
            public const POST_PACKAGE_UNINSTALL = 'post-package-uninstall';
        }
    }

    if (!class_exists(InstallerEvent::class)) {
        class InstallerEvent extends \Composer\EventDispatcher\Event
        {
            private $composer;
            private $io;
            private $devMode;
            private $executeOperations;
            private $transaction;

            public function __construct(string $eventName, \Composer\Composer $composer, \Composer\IO\IOInterface $io, bool $devMode, bool $executeOperations, \Composer\DependencyResolver\Transaction $transaction)
            {
                parent::__construct($eventName);
                $this->composer = $composer;
                $this->io = $io;
                $this->devMode = $devMode;
                $this->executeOperations = $executeOperations;
                $this->transaction = $transaction;
            }

            public function getComposer()
            {
                return $this->composer;
            }

            public function getIO()
            {
                return $this->io;
            }

            public function isDevMode()
            {
                return $this->devMode;
            }

            public function isExecutingOperations()
            {
                return $this->executeOperations;
            }

            public function getTransaction()
            {
                return $this->transaction;
            }
        }
    }

    if (!class_exists(InstallerEvents::class)) {
        class InstallerEvents
        {
            public const PRE_OPERATIONS_EXEC = 'pre-operations-exec';
        }
    }
}

namespace Composer\Package {
    if (!class_exists(Locker::class)) {
        /** composer.lock, read-only. */
        class Locker
        {
            private $path;
            private $fresh;

            public function __construct(string $path, bool $fresh)
            {
                $this->path = $path;
                $this->fresh = $fresh;
            }

            public function isLocked()
            {
                return is_file($this->path) && isset($this->getLockData()['packages']);
            }

            public function isFresh()
            {
                return $this->fresh;
            }

            public function getLockData()
            {
                $data = is_file($this->path) ? json_decode((string) file_get_contents($this->path), true) : null;
                if (!is_array($data)) {
                    throw new \LogicException('No lockfile found. Unable to read locked packages');
                }

                return $data;
            }

            public function __call($name, $arguments)
            {
                throw \Komposer\Script\Unsupported::method('Locker::'.$name);
            }
        }
    }
}

namespace Composer {
    if (!class_exists(Config::class)) {
        /** The project's config as komposer resolved it. */
        class Config
        {
            public const SOURCE_DEFAULT = 'default';
            public const SOURCE_COMMAND = 'command';
            public const SOURCE_UNKNOWN = 'unknown';
            public const RELATIVE_PATHS = 1;

            private $config;
            private $relative;
            private $repositories;

            public function __construct(array $config, array $relative, array $repositories)
            {
                $this->config = $config;
                $this->relative = $relative;
                $this->repositories = $repositories;
            }

            public function get(string $key, int $flags = 0)
            {
                if (($flags & self::RELATIVE_PATHS) && array_key_exists($key, $this->relative)) {
                    return $this->relative[$key];
                }

                return $this->config[$key] ?? null;
            }

            public function has(string $key)
            {
                return array_key_exists($key, $this->config);
            }

            public function all(int $flags = 0)
            {
                $all = ['repositories' => $this->repositories, 'config' => []];
                foreach (array_keys($this->config) as $key) {
                    $all['config'][$key] = $this->get($key, $flags);
                }

                return $all;
            }

            public function raw()
            {
                return ['repositories' => $this->repositories, 'config' => $this->config];
            }

            public function getRepositories()
            {
                return $this->repositories;
            }

            /** komposer handles `Composer\Config::disableProcessTimeout` scripts itself; inside PHP it has nothing to change. */
            public static function disableProcessTimeout()
            {
            }

            public function __call($name, $arguments)
            {
                throw \Komposer\Script\Unsupported::method('Config::'.$name);
            }
        }
    }

    if (!class_exists(PartialComposer::class)) {
        class PartialComposer
        {
            protected $package;
            protected $config;
            protected $repositoryManager;
            protected $installationManager;
            protected $eventDispatcher;

            public function getPackage()
            {
                return $this->package;
            }

            public function setPackage(Package\RootPackageInterface $package)
            {
                $this->package = $package;
            }

            public function getConfig()
            {
                return $this->config;
            }

            public function setConfig(Config $config)
            {
                $this->config = $config;
            }

            public function getRepositoryManager()
            {
                return $this->repositoryManager;
            }

            public function setRepositoryManager(Repository\RepositoryManager $manager)
            {
                $this->repositoryManager = $manager;
            }

            public function getInstallationManager()
            {
                return $this->installationManager;
            }

            public function setInstallationManager(Installer\InstallationManager $manager)
            {
                $this->installationManager = $manager;
            }

            public function getEventDispatcher()
            {
                return $this->eventDispatcher;
            }

            public function setEventDispatcher(EventDispatcher\EventDispatcher $eventDispatcher)
            {
                $this->eventDispatcher = $eventDispatcher;
            }

            public function __call($name, $arguments)
            {
                throw \Komposer\Script\Unsupported::method('Composer::'.$name);
            }
        }
    }

    if (!class_exists(Composer::class)) {
        class Composer extends PartialComposer
        {
            public const VERSION = '2.10.3';
            public const BRANCH_ALIAS_VERSION = '';
            public const RELEASE_DATE = '2026-08-27 13:34:23';
            public const SOURCE_VERSION = '';
            public const RUNTIME_API_VERSION = '2.2.2';

            protected $locker;

            public static function getVersion()
            {
                return self::VERSION;
            }

            public function getLocker()
            {
                return $this->locker;
            }

            public function setLocker(Package\Locker $locker)
            {
                $this->locker = $locker;
            }
        }
    }
}

namespace {
    use Composer\IO\IOInterface;

    [, $payloadFile, $resultFile] = $argv;
    $payload = json_decode((string) file_get_contents($payloadFile), true);
    $GLOBALS['komposer_payload'] = $payload;

    // what bin/composer and Composer's Application set up before running a command
    setlocale(LC_ALL, 'C');
    error_reporting(E_ALL);
    if (function_exists('ini_set')) {
        @ini_set('display_errors', ('' === ini_get('error_log') && (bool) ini_get('log_errors')) ? '0' : 'stderr');
        if ($memoryLimit = getenv('COMPOSER_MEMORY_LIMIT')) {
            @ini_set('memory_limit', $memoryLimit);
        } else {
            $memoryLimit = trim(ini_get('memory_limit'));
            $bytes = (int) $memoryLimit;
            switch (strtolower(substr($memoryLimit, -1, 1))) {
                case 'g':
                    $bytes *= 1024;
                    // no break
                case 'm':
                    $bytes *= 1024;
                    // no break
                case 'k':
                    $bytes *= 1024;
            }
            if ($memoryLimit != -1 && $bytes < 1024 * 1024 * 1536) {
                @ini_set('memory_limit', '1536M');
            }
        }
    }

    $io = new Composer\IO\ConsoleIO($payload['io']['verbosity'], $payload['io']['decorated'], $payload['io']['interactive']);
    $deprecationsShown = 0;
    // Composer\Util\ErrorHandler: warnings and notices end the script, deprecations are reported
    set_error_handler(static function (int $level, string $message, string $file, int $line) use ($io, &$deprecationsShown): bool {
        $deprecation = $level === E_DEPRECATED || $level === E_USER_DEPRECATED;
        if (!$deprecation && 0 === (error_reporting() & $level)) {
            return true;
        }
        if (!$deprecation) {
            if (in_array($level, [E_WARNING, E_USER_WARNING], true) && false !== strpos($message, 'should either be used or intentionally ignored by casting it as (void)')) {
                $io->writeError('<warning>Ignored new PHP warning but it should be reported and fixed: '.$message.' in '.$file.':'.$line.'</warning>');

                return true;
            }
            throw new ErrorException($message, 0, $level, $file, $line);
        }
        if ($deprecationsShown > 0 && !$io->isVerbose()) {
            if ($deprecationsShown === 1) {
                $io->writeError('<warning>More deprecation notices were hidden, run again with `-v` to show them.</warning>');
                $deprecationsShown = 2;
            }

            return true;
        }
        $deprecationsShown = 1;
        $io->writeError('<warning>Deprecation Notice: '.$message.' in '.$file.':'.$line.'</warning>');

        return true;
    });

    $outcome = ['return' => 0, 'stopped' => false];
    try {
        $project = $payload['project'];
        $toPackage = static function (array $data) {
            return new Composer\Package\CompletePackage($data);
        };
        $packages = array_map($toPackage, $project['packages']);
        $localRepo = new Composer\Repository\InstalledFilesystemRepository($packages, $project['devMode'], $project['devPackageNames']);

        $composer = new Composer\Composer();
        $composer->setPackage(new Composer\Package\RootPackage($project['root']));
        $composer->setConfig(new Composer\Config($project['config'], $project['configRelative'], $project['repositories']));
        $composer->setRepositoryManager(new Composer\Repository\RepositoryManager($localRepo));
        $composer->setInstallationManager(new Composer\Installer\InstallationManager($project['config']['vendor-dir']));
        $composer->setEventDispatcher(new Composer\EventDispatcher\EventDispatcher());
        $composer->setLocker(new Composer\Package\Locker($project['lockFile'], $project['lockFresh']));

        $e = $payload['event'];
        $operation = static function (array $op) use ($toPackage) {
            switch ($op['type']) {
                case 'update':
                    return new Composer\DependencyResolver\Operation\UpdateOperation($toPackage($op['initial']), $toPackage($op['target']));
                case 'uninstall':
                    return new Composer\DependencyResolver\Operation\UninstallOperation($toPackage($op['package']));
                default:
                    return new Composer\DependencyResolver\Operation\InstallOperation($toPackage($op['package']));
            }
        };
        $operations = array_map($operation, $e['operations']);
        if ($e['kind'] === 'package') {
            $event = new Composer\Installer\PackageEvent($e['name'], $composer, $io, $e['devMode'], $localRepo, $operations, $operation($e['operation']));
        } elseif ($e['kind'] === 'installer') {
            $event = new Composer\Installer\InstallerEvent($e['name'], $composer, $io, $e['devMode'], $e['executeOperations'], new Composer\DependencyResolver\Transaction($operations));
        } else {
            $event = new Composer\Script\Event($e['name'], $composer, $io, $e['devMode'], $e['args'], $e['flags'] ?: []);
        }

        // EventDispatcher::makeAutoloader(): every package's PSR-0/PSR-4 prefixes and class map
        if (!class_exists('Composer\Autoload\ClassLoader', false)) {
            require __DIR__.'/ClassLoader.php';
        }
        $loader = new Composer\Autoload\ClassLoader($project['config']['vendor-dir']);
        foreach ($project['loader']['psr-0'] as [$prefix, $paths]) {
            $loader->add($prefix, $paths);
        }
        foreach ($project['loader']['psr-4'] as [$prefix, $paths]) {
            $loader->addPsr4($prefix, $paths);
        }
        $loader->addClassMap($project['loader']['classmap']);
        $loader->register(false);

        $listener = $payload['listener'];
        // `composer exec` and `composer <script>` do not echo what they run
        $echo = $e['name'] !== '__exec_command' && empty($e['flags']['script-alias-input']);
        if (false === strpos($listener, ' ') && false !== strpos($listener, '::')) {
            $className = substr($listener, 0, strpos($listener, '::'));
            $methodName = substr($listener, strpos($listener, '::') + 2);
            if (!class_exists($className)) {
                $io->writeError('<warning>Class '.$className.' is not autoloadable, can not call '.$event->getName().' script</warning>', true, IOInterface::QUIET);
            } elseif (!is_callable($listener)) {
                $io->writeError('<warning>Method '.$listener.' is not callable, can not call '.$event->getName().' script</warning>', true, IOInterface::QUIET);
            } else {
                try {
                    if ($io->isVerbose()) {
                        $io->writeError(sprintf('> %s: %s::%s', $event->getName(), $className, $methodName));
                    } elseif ($echo) {
                        $io->writeError(sprintf('> %s::%s', $className, $methodName));
                    }
                    $outcome['return'] = false === $className::$methodName($event) ? 1 : 0;
                } catch (Exception $ex) {
                    $io->writeError('<error>'.sprintf('Script %s handling the %s event terminated with an exception', $listener, $event->getName()).'</error>', true, IOInterface::QUIET);
                    throw $ex;
                }
            }
        } else {
            // a Symfony Console command class
            $className = $listener;
            $reserved = defined('Composer\Script\ScriptEvents::'.str_replace('-', '_', strtoupper($event->getName())));
            if (!class_exists($className)) {
                $io->writeError('<warning>Class '.$className.' is not autoloadable, can not call '.$event->getName().' script</warning>', true, IOInterface::QUIET);
            } elseif (!is_a($className, 'Symfony\Component\Console\Command\Command', true)) {
                $io->writeError('<warning>Class '.$className.' does not extend Symfony\Component\Console\Command\Command, can not call '.$event->getName().' script</warning>', true, IOInterface::QUIET);
            } elseif ($reserved) {
                $io->writeError('<warning>You cannot bind '.$event->getName().' to a Command class, use a non-reserved name</warning>', true, IOInterface::QUIET);
            } else {
                $app = new Symfony\Component\Console\Application();
                $app->setCatchExceptions(false);
                if (method_exists($app, 'setCatchErrors')) {
                    $app->setCatchErrors(false);
                }
                $app->setAutoExit(false);
                $cmd = new $className($event->getName());
                if (method_exists($app, 'addCommand')) {
                    $app->addCommand($cmd);
                } else {
                    $app->add($cmd);
                }
                $app->setDefaultCommand((string) $cmd->getName(), true);
                $verbosity = [IOInterface::QUIET => 16, IOInterface::NORMAL => 32, IOInterface::VERBOSE => 64, IOInterface::VERY_VERBOSE => 128, IOInterface::DEBUG => 256][$payload['io']['verbosity']];
                $output = new Symfony\Component\Console\Output\ConsoleOutput($verbosity, $payload['io']['decorated']);
                $args = implode(' ', array_map(static function ($arg) {
                    return "'".str_replace("'", "'\\''", (string) $arg)."'";
                }, $e['args']));
                try {
                    $outcome['return'] = $app->run(new Symfony\Component\Console\Input\StringInput($e['flags']['script-alias-input'] ?? $args), $output);
                } catch (Exception $ex) {
                    $io->writeError('<error>'.sprintf('Script %s handling the %s event terminated with an exception', $listener, $event->getName()).'</error>', true, IOInterface::QUIET);
                    throw $ex;
                }
            }
        }
        $outcome['stopped'] = $event->isPropagationStopped();
    } catch (Throwable $ex) {
        Komposer\Script\Renderer::render($ex, $io);
        $outcome = ['exception' => $ex->getCode()];
    }
    file_put_contents($resultFile, json_encode($outcome));
}
