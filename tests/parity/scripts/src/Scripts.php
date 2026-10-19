<?php
namespace Acme;

use Composer\Installer\InstallerEvent;
use Composer\Installer\PackageEvent;
use Composer\Script\Event;

class Scripts
{
    public static function preOps(InstallerEvent $event)
    {
        $event->getIO()->write('<info>pre-ops</info>: '.count($event->getTransaction()->getOperations()).' operations, executing: '.var_export($event->isExecutingOperations(), true));
    }

    public static function packageInstalled(PackageEvent $event)
    {
        $package = $event->getOperation()->getPackage();
        $event->getIO()->write('installed '.$package->getName().' ('.$event->getOperation()->getOperationType().', '.count($event->getOperations()).' ops, '.$package->getType().')');
    }

    public static function postInstall(Event $event)
    {
        $composer = $event->getComposer();
        $io = $event->getIO();
        $io->write('vendor-dir '.basename($composer->getConfig()->get('vendor-dir')).', relative '.$composer->getConfig()->get('vendor-dir', \Composer\Config::RELATIVE_PATHS));
        $io->write('root '.$composer->getPackage()->getName().', extra '.$composer->getPackage()->getExtra()['greeting'].', type '.$composer->getPackage()->getType());
        $repo = $composer->getRepositoryManager()->getLocalRepository();
        $names = array_map(function ($p) { return $p->getName(); }, $repo->getCanonicalPackages());
        sort($names);
        $io->write('installed: '.implode(', ', $names));
        $io->write('dev mode '.var_export($event->isDevMode(), true).', event '.$event->getName());
        $log = $repo->findPackage('psr/log', '*');
        $io->write('psr/log at '.basename(dirname($composer->getInstallationManager()->getInstallPath($log))).'/'.basename($composer->getInstallationManager()->getInstallPath($log)).', stability '.$log->getStability());
        $io->writeError('<comment>to stderr</comment>');
        $io->write('verbose only', true, \Composer\IO\IOInterface::VERBOSE);
    }

    public static function dump(Event $event)
    {
        $event->getIO()->write('dumped, autoload.php exists: '.var_export(is_file($event->getComposer()->getConfig()->get('vendor-dir').'/autoload.php'), true));
    }

    public static function args(Event $event)
    {
        $event->getIO()->write('args '.json_encode($event->getArguments()).', flags '.json_encode($event->getFlags()));
    }

    public static function returnsFalse(Event $event)
    {
        $event->getIO()->write('returning false');

        return false;
    }

    public static function stops(Event $event)
    {
        $event->getIO()->write('stopping');
        $event->stopPropagation();
    }

    public static function boom(Event $event)
    {
        throw new \RuntimeException("it went\nboom", 3);
    }

    public static function notice(Event $event)
    {
        $a = [];

        return $a['missing'];
    }
}
