# Lotus

## About the Project

Lotus is a tool that runs test cases against a Logstash pipeline. Note though,
that Lotus is very early on, currently. Expect bumps and bruises.

## Getting Started

Lotus is not primarily designed to be installed on your system. Instead, it
integrates with [Pre-Commit](https://pre-commit.com/) (we'll see how to set it
up later). Lotus _can_ be installed on its own, but you need to have an
installation of Rust / Cargo on your system.

Its aim is to help you write tests for your Logstash pipelines. Lotus assumes
that your Logstash rules reside in the `rules` subdirectory of the root of your
project, and that your test cases reside in the `tests` subdirectory of the
root of your project. Each test case is placed in its own subdirectory under
`tests` (you can name it freely), and Lotus expects two files to be present:

* `input.json` contains an example of an event as it would arrive in Logstash
  from one of your shippers (either a Beat or something else).
* `expected.json` contains the **expected** output of your Logstash pipeline.
  The **actual** output of your pipeline is then compared against it to
  determine test case success or failure.

Lotus provides a Pre-Commit hook for the stage `pre-push`, because it takes a
long time for Logstash to start up, and you would not want to do that on every
commit.

### Prerequisites

First, you must install Docker on your system. Since Docker comes in many
different flavours, you should probably follow the [Getting
Started](https://www.docker.com/get-started/) guide. Lotus might work with
Podman as well, but that hasn't been tested. **Note that Lotus relies on the
availability of the Docker API on the host machine.**

As a second requirement, install Pre-Commit on your system. See [Pre-Commit
Installation](https://pre-commit.com/#install) for details.

Then, create a Pre-Commit configuration within your Logstash pipeline
repository, using the following example:

```yaml
# .pre-commit-config.yaml
repos:
  - repo: https://gitlab.com/nausicaea/lotus
    rev: v0.4.1
    hooks:
      - id: lotus
```

Finally, continue following the [Pre-Commit
Quickstart](https://pre-commit.com/#quick-start) guide.

### How does it work?

1. Lotus first searches for your Logstash rules (anything in the subdirectory
   `rules` that ends in `.conf`).
2. It subsequently collects all test cases from subdirectories of the `tests`
   directory in your project.
4. It then builds a Docker image from a Logstash configuration and the rules of
   your project. Your rule files are sorted lexicographically, bracketed by
   Lotus' own Logstash `input` and `output` rules, and concatenated into a
   single file.
5. Given that Docker image, Lotus then starts a new Docker container, and waits
   for Logstash to be ready.
6. If all is well, the following operations are run for each of your test cases:
    1. Lotus sends an HTTP POST request to Logstash containing your
       `input.json` data.
    2. It then waits for an HTTP POST request from Logstash in another thread
       containing the output of your pipeline.
    3. Lastly, it compares the output with the expected output data you
       provided in `expected.json`.
7. Any errors are reported as soon as they happen. The first error terminates
   Lotus.

## License

Licensed under either of

 * Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license
   ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
