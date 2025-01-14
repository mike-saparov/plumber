use std::io::Write;
use std::path::{Path, PathBuf};
use std::fs;
use std::process::{Child, Stdio, Command};
use std::os::unix::process::CommandExt;
use log::error;

const LOGGING_DIR: &str = "/tmp/plumber/log";
const METADATA_DIR: &str = "/tmp/plumber/lib";

#[derive(Debug, PartialEq)]
struct PipelineCommand {
    name: String,
    args: Vec<String>,
}

impl PipelineCommand {
    pub fn new(mut cmd: Vec<String>) -> Self {
        let name = cmd.remove(0);
        let args = cmd;

        PipelineCommand {
            name,
            args
        }
    }
}

pub struct Pipeline {
    name: String,
    raw_pipeline: String,
    commands: Vec<PipelineCommand>,
    jobs: Vec<Child>,
    metadata_dir: PathBuf,
    logging_dir: PathBuf,
}

#[derive(Debug)]
pub enum PipelineError {
    FileNotFound,
    Other
}

impl From<std::io::Error> for PipelineError {
    fn from(value: std::io::Error) -> Self {
        match value.kind() {
            std::io::ErrorKind::NotFound => PipelineError::FileNotFound,
            _ => PipelineError::Other,
        }
    }
}

impl Pipeline {
    pub fn stop(name: &str) -> Result<(), PipelineError> {
        let metadata_dir = Path::new(METADATA_DIR).join(&name);
        let first_job_pid = fs::read_to_string(metadata_dir.join(".pid"))?;

        log::debug!("{name}: stopping first process in pipeline => kill -SIGTERM {first_job_pid}");
        let _ = Command::new("kill")
            .arg("-SIGTERM")
            .arg(&first_job_pid)
            .status()?;

        Ok(())
    }

    pub fn get_name(&self) -> String {
        self.name.clone()
    }

    pub fn get_first_pid(&self) -> String {
        self.jobs.first()
            .unwrap()
            .id()
            .to_string()
    }

    fn parse_raw_pipeline(raw_pipeline: &str) -> Vec<PipelineCommand> {
        let split_on_pipe = raw_pipeline.split('|'); // split pipes

        let split_on_whitespace: Vec<Vec<String>> = split_on_pipe.map(|cmd_string|
            shlex::split(cmd_string)
            .unwrap_or_default())
            .collect();

        let commands: Vec<PipelineCommand> = split_on_whitespace
            .into_iter().map(|cmd|
            PipelineCommand::new(cmd))
            .collect();

        assert!(!commands.is_empty(), "unable to parse commands - empty list: {}", raw_pipeline);

        commands
    }

    pub fn new(name: String, raw_pipeline: String) -> Result<Self, PipelineError> {
        let commands = Pipeline::parse_raw_pipeline(&raw_pipeline);
        let metadata_dir = Path::new(METADATA_DIR).join(&name);
        let logging_dir = Path::new(LOGGING_DIR).join(&name);
        create_dir_with_nice_error(&metadata_dir)?;
        create_dir_with_nice_error(&logging_dir)?;

        Ok(Pipeline {
            name,
            raw_pipeline,
            commands,
            jobs: Vec::new(),
            metadata_dir,
            logging_dir
        })
    }

    pub fn new_from_file(path: &Path) -> Result<Self, PipelineError> {
        let name = path.file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();

        let raw_pipeline = fs::read_to_string(path)?;

        Self::new(name, raw_pipeline)
    }

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
            .process_group(0)
            .spawn()
            .expect(&format!("Failed to spawn command: {} {}", name, args.join(" ")))
    }

    fn spawn_all(&mut self) {
        let mut prev_stdout = Stdio::inherit();

        let commands_except_last = &self.commands[..self.commands.len() - 1];
        for cmd in commands_except_last.iter() {
            let stderr_out = fs::File::create(&self.logging_dir
                        .join(&cmd.name)
                        .with_extension("stderr.log"))
                        .unwrap();

            let stderr_out = Stdio::from(stderr_out);

            let mut child = Self::spawn_process(
                &cmd.name, &cmd.args,
                prev_stdout, Stdio::piped(), stderr_out
            );
            prev_stdout = Stdio::from(child.stdout.take().unwrap());
            self.jobs.push(child);
        }

        // this is to pipe the stdout of the last command to the parent process
        let last_cmd = self.commands.last().unwrap();

        let stderr_out = fs::File::create(&self.logging_dir
            .join(&last_cmd.name)
            .with_extension("stderr.log")
        ).unwrap();

        let stderr_out = Stdio::from(stderr_out);

        let child = Self::spawn_process(
            &last_cmd.name, &last_cmd.args,
            prev_stdout, Stdio::inherit(), stderr_out
        );
        self.jobs.push(child);
    }

    pub fn run(mut self) {
        log::info!("{}: executing pipeline => '{}'", &self.name, &self.raw_pipeline.trim());
        log::info!("{}: logging command stderr to => '{}'", &self.name, &self.logging_dir.join("*.stderr.log").display());
        self.spawn_all();

        let first_job_pid = self.get_first_pid();

        log::debug!("{}: pid of first job in pipeline is {}", &self.name, &first_job_pid);

        let mut pid_file = fs::File::create(&self.metadata_dir.join(".pid")).unwrap();
        pid_file.write_all(first_job_pid.as_bytes()).unwrap();
        pid_file.flush().unwrap();

        for jobs in &mut self.jobs {
            jobs.wait().unwrap();
        }

        drop(pid_file);
        fs::remove_file(&self.metadata_dir.join(".pid")).unwrap();
    }
}

fn create_dir_with_nice_error(dir: &Path) -> Result<(), std::io::Error> {
    match fs::create_dir_all(dir) {
        Ok(_) => Ok(()),
        Err(e) => match e.kind() {
            std::io::ErrorKind::PermissionDenied => {
                error!("plumber requires permission to write in {}",
                       dir.parent().unwrap().display());
                error!("recommended to have user that executes plumber to own this directory");
                Err(e)
            },
            _ => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_dir_permissions() {
        let path = Path::new(LOGGING_DIR);
        let test_dir = "asdf_plumber_test";
        create_dir_with_nice_error(&path.join(test_dir)).unwrap();
        fs::remove_dir(&path.join(test_dir)).unwrap();
    }

    #[test]
    fn metadata_dir_permissions() {
        let path = Path::new(METADATA_DIR);
        let test_dir = "asdf_plumber_test";
        let path = &path.join(test_dir);
        create_dir_with_nice_error(path).unwrap();
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn writing_pid_file() {
        let path = Path::new(METADATA_DIR);
        let test_dir = "asdf_plumber_test_2";
        let path = &path.join(test_dir);
        create_dir_with_nice_error(path).unwrap();

        let mut pid_file = fs::File::create(path.join(".pid")).unwrap();
        pid_file.write_all("12345".as_bytes()).unwrap();
        pid_file.flush().unwrap();
        drop(pid_file);
        fs::remove_file(&path.join(".pid")).unwrap();
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn parse_raw_pipeline() {
        let pipeline = "cat file -a -v | pv --force |   oops_two_spaces  |      grep 'a' ";
        let res = vec![
            PipelineCommand {
                name: "cat".to_string(),
                args: vec![
                    "file".to_string(),
                    "-a".to_string(),
                    "-v".to_string(),
                ],
            },
            PipelineCommand {
                name: "pv".to_string(),
                args: vec![
                    "--force".to_string(),
                ],
            },
            PipelineCommand {
                name: "oops_two_spaces".to_string(),
                args: vec![],
            },
            PipelineCommand {
                name: "grep".to_string(),
                args: vec![
                    "a".to_string(),
                ],
            },
        ];

        assert_eq!(res, Pipeline::parse_raw_pipeline(pipeline));
    }


}
