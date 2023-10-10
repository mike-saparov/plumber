use std::path::PathBuf;
use std::process::{Command, Stdio, Child};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::os::unix::process::CommandExt;
use std::thread;
use std::time::Duration;
use std::fs::File;
use nix::unistd::Pid;
use nix::sys::signal;
use nix::sys::signal::Signal;

// Convenience structure to split command vector (cat a_file) into a
// command name (cat) and arguments ([a_file]).
struct PipelineCommand {
    name: String,
    args: Vec<String>,
}

impl PipelineCommand {
    pub fn new(mut cmd: Vec<String>) -> PipelineCommand {
        let name = cmd.remove(0);
        let args = cmd;

        PipelineCommand {
            name,
            args
        }
    }
}

// Future proof if pipeline needs more than a vector of commands as input.
// Maybe some sort of settings in the pipeline file?
pub struct PipelineInput {
    _input_string: String,
    metadata_dir: PathBuf,
    commands: Vec<PipelineCommand>,
}

impl PipelineInput {
    pub fn new(input_string: String, metadata_dir: PathBuf) -> PipelineInput {
        let split_on_pipe = input_string.split('|'); // split pipes

        let split_on_whitespace: Vec<Vec<String>> = split_on_pipe.map(|cmd_string|
            shlex::split(cmd_string)
            .unwrap_or_default())
            .collect();

        let commands: Vec<PipelineCommand> = split_on_whitespace
            .into_iter().map(|cmd|
            PipelineCommand::new(cmd))
            .collect();

        PipelineInput {
            _input_string: input_string,
            metadata_dir,
            commands
        }
    }
}

pub struct Pipeline {
    shutdown: Arc<AtomicBool>,
    jobs: Vec<Child>,
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // if we get term signal, kill ONLY the first job.
        // this ensures all data in the pipeline is processed to the end.
        if self.shutdown.load(Ordering::Relaxed) {
            eprintln!("exiting gracefully...");
            let pid = self.jobs.first().unwrap().id();
            signal::kill(Pid::from_raw(pid.try_into().unwrap()), Signal::SIGTERM).unwrap();
        }

        for jobs in &mut self.jobs {
            jobs.wait().unwrap();
        }
    }
}

impl Pipeline {
    fn spawn_process(
        name: &String,
        args: &Vec<String>,
        stdin: Stdio,
        stdout: Stdio,
        stderr: Stdio) -> Child {
        let mut child = Command::new(name);

        child.args(args);

        child
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr)
            //.process_group(0)
            .spawn()
            .expect(&format!("Failed to spawn command: {} {}", name, args.join(" ")))
    }

    pub fn new(input: &PipelineInput, shutdown: Arc<AtomicBool>) -> Pipeline {
        let mut jobs = Vec::new();
        let mut prev_stdout = Stdio::null();

        let mut stderr_path = input.metadata_dir.clone();
        stderr_path.push("tmp");

        let commands_exc_last = &input.commands[..input.commands.len() - 1];

        if !commands_exc_last.is_empty() {

            for cmd in commands_exc_last.iter() {
                let stderr = File::create(
                    stderr_path
                    .with_file_name(&cmd.name)
                    .with_extension("stderr.log")
                ).unwrap();

                let stderr = Stdio::from(stderr);
                let mut child = Self::spawn_process(
                    &cmd.name, &cmd.args,
                    prev_stdout, Stdio::piped(), stderr
                );
                prev_stdout = Stdio::from(child.stdout.take().unwrap());
                jobs.push(child);
            }
        }

        // this is to pipe the stdout of the last command to the parent process
        let last_cmd = input.commands.last().unwrap();

        let stderr = File::create(
            stderr_path
            .with_file_name(&last_cmd.name)
            .with_extension("stderr.log")
        ).unwrap();

        let stderr = Stdio::from(stderr);

        let child = Self::spawn_process(
            &last_cmd.name, &last_cmd.args,
            prev_stdout, Stdio::inherit(), stderr
        );
        jobs.push(child);

        Pipeline { shutdown, jobs }
    }

    fn busy_wait_and_sleep(&mut self, seconds: u64) -> bool {
        thread::sleep(Duration::from_secs(seconds));
        match self.jobs.last_mut().unwrap().try_wait() {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(err) => panic!("{}", err)
        }
    }

    pub fn run(mut self) {
        while !self.shutdown.load(Ordering::Relaxed) {
            if self.busy_wait_and_sleep(2) { break }
        }
    }
}
