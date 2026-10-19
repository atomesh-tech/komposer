<?php
namespace Acme;

use Symfony\Component\Console\Command\Command;
use Symfony\Component\Console\Input\InputArgument;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Output\OutputInterface;

class GreetCommand extends Command
{
    protected function configure(): void
    {
        $this->addArgument('who', InputArgument::OPTIONAL, '', 'world');
    }

    protected function execute(InputInterface $input, OutputInterface $output): int
    {
        $output->writeln('hello '.$input->getArgument('who'));

        return 0;
    }
}
