// Copyright (c) 2018  Brendan Molloy <brendan@bbqsrc.net>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.
#![feature(async_await)]
#![feature(async_closure)]

pub extern crate gherkin;
pub extern crate globwalk;

pub mod cli;
mod hashable_regex;
mod output;
mod panic_trap;

use std::collections::HashMap;
use std::convert::TryFrom;
use std::fmt;
use std::fs::File;
use std::io::{stderr, Read, Write};
use std::path::PathBuf;

use gherkin::Feature;
pub use gherkin::{Scenario, Step, StepType};
use regex::Regex;

use crate::hashable_regex::HashableRegex;
pub use crate::output::default::DefaultOutput;
use crate::output::OutputVisitor;
use crate::panic_trap::{PanicDetails, PanicTrap};

use futures::future::{BoxFuture, Future, FutureExt};
use std::pin::Pin;

pub trait World: Default + Clone {}

type HelperFn = fn(&Scenario) -> ();

use std::panic::{UnwindSafe, AssertUnwindSafe};

type TestSyncFn<W> = fn(&mut W, &Step) -> ();
type RegexTestFn<W> = fn(&mut W, &[String], &Step) -> ();
type RegexTestSyncFn<W> = fn(&mut W, &[String], &Step) -> ();
type TestFn<W> = fn(W, Step) -> TestFuture;
// TODO 
pub struct TestFuture {
    future: BoxFuture<'static, ()>
}

impl UnwindSafe for TestFuture {}

use futures::task::{Poll, Context};
use std::panic::{catch_unwind};

use pin_utils::unsafe_pinned;
impl TestFuture {
    unsafe_pinned!(future: BoxFuture<'static, ()>);

    pub fn new(f: impl Future<Output = ()> + Send + 'static) -> Self {
        TestFuture { future: f.boxed() }
    }
}


#[must_use = "futures do nothing unless you `.await` or poll them"]
impl Future for TestFuture {
    type Output = Result<(), Box<dyn std::any::Any + Send>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        catch_unwind(AssertUnwindSafe(|| self.future().poll(cx)))?.map(Ok)
    }
}

// type RegexTestFn<W> = fn(&mut W, &[String], &Step) -> BoxFuture<'static, ()>;

// fn woot<W: World>() -> TestFn<W> {// fn(&mut W, &Step) -> Box<dyn Future<Output = ()>> {
//     Box::new(|world: &mut W, step: &Step| Box::new(async {
//         step.description;
//         42usize;
//     }))
// }

type TestAsyncBag<W> = HashMap<&'static str, TestFn<W>>;
type TestSyncBag<W> = HashMap<&'static str, TestSyncFn<W>>;
type RegexBag<W> = HashMap<HashableRegex, RegexTestFn<W>>;

#[derive(Default)]
pub struct Steps<W: World> {
    given: TestSyncBag<W>,
    when: TestSyncBag<W>,
    then: TestSyncBag<W>,
    regex: RegexSteps<W>,
    async_: AsyncSteps<W>,
}

#[derive(Default)]
struct RegexSteps<W: World> {
    given: RegexBag<W>,
    when: RegexBag<W>,
    then: RegexBag<W>,
}

impl<W: World> fmt::Debug for Steps<W> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("Steps")
            .field("given", &self.given.keys())
            .field("when", &self.when.keys())
            .field("then", &self.then.keys())
            .field("regex", &self.regex)
            .field("async", &self.async_)
            .finish()
    }
}

impl<W: World> fmt::Debug for RegexSteps<W> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("RegexSteps")
            .field("given", &self.given.keys())
            .field("when", &self.when.keys())
            .field("then", &self.then.keys())
            .finish()
    }
}

impl<W: World> fmt::Debug for AsyncSteps<W> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("AsyncSteps")
            .field("given", &self.given.keys())
            .field("when", &self.when.keys())
            .field("then", &self.then.keys())
            .finish()
    }
}

#[derive(Default)]
struct AsyncSteps<W: World> {
    given: TestAsyncBag<W>,
    when: TestAsyncBag<W>,
    then: TestAsyncBag<W>,
}

// impl<W: World> Default for AsyncSteps<W> {
//     fn default() -> Self {
//         AsyncSteps {
//             given: HashMap::new(),
//             when: HashMap::new(),
//             then: HashMap::new(),
//         }
//     }
// }

enum TestCaseType<'a, W: 'a + World> {
    Normal(&'a TestSyncFn<W>),
    Async(&'a TestFn<W>),
    Regex(&'a RegexTestFn<W>, Vec<String>),
}

pub enum TestResult {
    Skipped,
    Unimplemented,
    Pass,
    Fail(PanicDetails, Vec<u8>, Vec<u8>),
}

pub struct StepsBuilder<W>
where
    W: World,
{
    steps: Steps<W>,
}

// impl<W: World + Send> StepsBuilder<W> {
// }

impl<W: World> StepsBuilder<W> {
    pub fn new() -> StepsBuilder<W> {
        StepsBuilder {
            steps: Steps::default()
        }
    }

    pub fn add_normal_async(
        &mut self,
        ty: StepType,
        name: &'static str,
        test_fn: TestFn<W>,
    ) -> &mut Self {
        self.steps.async_bag_mut_for(ty).insert(name, test_fn);
        self
    }

    pub fn given_async(&mut self, name: &'static str, test_fn: TestFn<W>) -> &mut Self {
        self.add_normal_async(StepType::Given, name, test_fn);
        self
    }

    pub fn given(&mut self, name: &'static str, test_fn: TestSyncFn<W>) -> &mut Self {
        self.add_normal(StepType::Given, name, test_fn);
        self
    }

    pub fn when(&mut self, name: &'static str, test_fn: TestSyncFn<W>) -> &mut Self {
        self.add_normal(StepType::When, name, test_fn);
        self
    }

    pub fn then(&mut self, name: &'static str, test_fn: TestSyncFn<W>) -> &mut Self {
        self.add_normal(StepType::Then, name, test_fn);
        self
    }

    pub fn given_regex(&mut self, regex: &'static str, test_fn: RegexTestSyncFn<W>) -> &mut Self {
        self.add_regex(StepType::Given, regex, test_fn);
        self
    }

    pub fn when_regex(&mut self, regex: &'static str, test_fn: RegexTestSyncFn<W>) -> &mut Self {
        self.add_regex(StepType::When, regex, test_fn);
        self
    }

    pub fn then_regex(&mut self, regex: &'static str, test_fn: RegexTestSyncFn<W>) -> &mut Self {
        self.add_regex(StepType::Then, regex, test_fn);
        self
    }

    pub fn add_normal(
        &mut self,
        ty: StepType,
        name: &'static str,
        test_fn: TestSyncFn<W>,
    ) -> &mut Self {
        self.steps.test_bag_mut_for(ty).insert(name, test_fn);
        self
    }

    pub fn add_regex(
        &mut self,
        ty: StepType,
        regex: &str,
        test_fn: RegexTestSyncFn<W>,
    ) -> &mut Self {
        let regex = Regex::new(regex)
            .unwrap_or_else(|_| panic!("`{}` is not a valid regular expression", regex));

        self.steps
            .regex_bag_mut_for(ty)
            .insert(HashableRegex(regex), test_fn);

        self
    }

    pub fn build(self) -> Steps<W> {
        self.steps
    }
}

impl<W: World + Default> Steps<W> {
    fn test_bag_for(&self, ty: StepType) -> &TestSyncBag<W> {
        match ty {
            StepType::Given => &self.given,
            StepType::When => &self.when,
            StepType::Then => &self.then,
        }
    }

    fn test_bag_mut_for(&mut self, ty: StepType) -> &mut TestSyncBag<W> {
        match ty {
            StepType::Given => &mut self.given,
            StepType::When => &mut self.when,
            StepType::Then => &mut self.then,
        }
    }

    fn async_bag_for(&self, ty: StepType) -> &TestAsyncBag<W> {
        match ty {
            StepType::Given => &self.async_.given,
            StepType::When => &self.async_.when,
            StepType::Then => &self.async_.then,
        }
    }

    fn async_bag_mut_for(&mut self, ty: StepType) -> &mut TestAsyncBag<W> {
        match ty {
            StepType::Given => &mut self.async_.given,
            StepType::When => &mut self.async_.when,
            StepType::Then => &mut self.async_.then,
        }
    }

    fn regex_bag_for(&self, ty: StepType) -> &RegexBag<W> {
        match ty {
            StepType::Given => &self.regex.given,
            StepType::When => &self.regex.when,
            StepType::Then => &self.regex.then,
        }
    }

    fn regex_bag_mut_for(&mut self, ty: StepType) -> &mut RegexBag<W> {
        match ty {
            StepType::Given => &mut self.regex.given,
            StepType::When => &mut self.regex.when,
            StepType::Then => &mut self.regex.then,
        }
    }

    fn test_type<'a>(&'a self, step: &Step) -> Option<TestCaseType<'a, W>> {
        if let Some(t) = self.test_bag_for(step.ty).get(&*step.value) {
            return Some(TestCaseType::Normal(t));
        }

        if let Some(t) = self.async_bag_for(step.ty).get(&*step.value) {
            return Some(TestCaseType::Async(t));
        }

        if let Some((regex, t)) = self
            .regex_bag_for(step.ty)
            .iter()
            .find(|(regex, _)| regex.is_match(&step.value))
        {
            let matches = regex
                .0
                .captures(&step.value)
                .unwrap()
                .iter()
                .map(|match_| {
                    match_
                        .map(|match_| match_.as_str().to_owned())
                        .unwrap_or_default()
                })
                .collect();

            return Some(TestCaseType::Regex(t, matches));
        }

        None
    }

    pub fn combine(iter: impl Iterator<Item = Self>) -> Self {
        let mut combined = Self::default();

        for steps in iter {
            combined.given.extend(steps.given);
            combined.when.extend(steps.when);
            combined.then.extend(steps.then);

            combined.regex.given.extend(steps.regex.given);
            combined.regex.when.extend(steps.regex.when);
            combined.regex.then.extend(steps.regex.then);

            combined.async_.given.extend(steps.async_.given);
            combined.async_.when.extend(steps.async_.when);
            combined.async_.then.extend(steps.async_.then);
        }

        combined
    }

    async fn run_test<'f>(
        &self,
        world: &'f mut W,
        test_type: TestCaseType<'_, W>,
        step: Step,
        suppress_output: bool,
    ) -> TestResult {
        let test_result = match test_type {
            TestCaseType::Normal(t) => PanicTrap::run(suppress_output, || t(world, &step)),
            TestCaseType::Regex(t, ref c) => PanicTrap::run(suppress_output, || t(world, c, &step)),
            TestCaseType::Async(t) => {
                let unwindable = t(world.clone(), step.clone())
                    .catch_unwind();
                let result = match unwindable.await {
                    Ok(unwind) => match unwind {
                        Ok(_) => Ok(()),
                        Err(e) => {
                            let payload = if let Some(s) = e.downcast_ref::<String>() {
                                s.clone().to_string()
                            } else if let Some(s) = e.downcast_ref::<&str>() {
                                s.to_string()
                            } else {
                                "Opaque panic payload".to_owned()
                            };
                            Err(PanicDetails { payload, location: "<async>:0:0".into() })
                        }
                    },
                    Err(e) => Err(PanicDetails { payload: "".into(), location: "".into() })
                };
                    //     println!("OK: {:?}", &x);
                    //     ()
                    // })
                    // .map_err(|e| {
                    //     println!("ERR: {:?}", &e);
                    //     PanicDetails { payload: "".into(), location: "".into() }
                    // });
                // println!("RESULT: {:?}", &result);
                PanicTrap { result, stdout: vec![], stderr: vec![] }
            }
        };

        // TestResult::Pass

        match test_result.result {
            Ok(_) => TestResult::Pass,
            Err(panic_info) => {
                if panic_info.payload.ends_with("cucumber test skipped") {
                    TestResult::Skipped
                } else {
                    TestResult::Fail(panic_info, test_result.stdout, test_result.stderr)
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_scenario(
        &self,
        feature: &gherkin::Feature,
        rule: Option<&gherkin::Rule>,
        scenario: &gherkin::Scenario,
        before_fns: &Option<&[HelperFn]>,
        after_fns: &Option<&[HelperFn]>,
        suppress_output: bool,
        mut output: impl OutputVisitor,
    ) -> bool {
        output.visit_scenario(rule, &scenario);

        if let Some(before_fns) = before_fns {
            for f in before_fns.iter() {
                f(&scenario);
            }
        }

        let mut world = {
            let panic_trap = PanicTrap::run(suppress_output, W::default);
            match panic_trap.result {
                Ok(v) => v,
                Err(panic_info) => {
                    eprintln!(
                        "Panic caught during world creation. Panic location: {}",
                        panic_info.location
                    );
                    if !panic_trap.stdout.is_empty() {
                        eprintln!("Captured output was:");
                        Write::write(&mut stderr(), &panic_trap.stdout).unwrap();
                    }
                    panic!(panic_info.payload);
                }
            }
        };

        let mut is_success = true;
        let mut is_skipping = false;

        let mut steps = vec![];

        if let Some(background) = feature.background.as_ref() {
            for step in background.steps.iter() {
                steps.push(step.to_owned());
            }
        }

        for step in scenario.steps.iter() {
            steps.push(step.clone());
        }

        for step in steps.into_iter() {
            output.visit_step(rule, &scenario, &step);

            let test_type = match self.test_type(&step) {
                Some(v) => v,
                None => {
                    output.visit_step_result(rule, &scenario, &step, &TestResult::Unimplemented);
                    if !is_skipping {
                        is_skipping = true;
                        output.visit_scenario_skipped(rule, &scenario);
                    }
                    continue;
                }
            };

            if is_skipping {
                output.visit_step_result(rule, &scenario, &step, &TestResult::Skipped);
            } else {
                let result = self
                    .run_test(&mut world, test_type, step.clone(), suppress_output)
                    .await;
                output.visit_step_result(rule, &scenario, &step, &result);
                match result {
                    TestResult::Pass => {}
                    TestResult::Fail(_, _, _) => {
                        is_success = false;
                        is_skipping = true;
                    }
                    _ => {
                        is_skipping = true;
                        output.visit_scenario_skipped(rule, &scenario);
                    }
                };
            }
        }

        if let Some(after_fns) = after_fns {
            for f in after_fns.iter() {
                f(&scenario);
            }
        }

        output.visit_scenario_end(rule, &scenario);

        is_success
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_scenarios(
        &self,
        feature: &gherkin::Feature,
        rule: Option<&gherkin::Rule>,
        scenarios: &[gherkin::Scenario],
        before_fns: Option<&[HelperFn]>,
        after_fns: Option<&[HelperFn]>,
        options: &cli::CliOptions,
        output: &mut impl OutputVisitor,
    ) -> bool {
        let mut futures = vec![];

        for scenario in scenarios {
            // If a tag is specified and the scenario does not have the tag, skip the test.
            let should_skip = match (&scenario.tags, &options.tag) {
                (Some(ref tags), Some(ref tag)) => !tags.contains(tag),
                _ => false,
            };

            if should_skip {
                continue;
            }

            // If regex filter fails, skip the test.
            if let Some(ref regex) = options.filter {
                if !regex.is_match(&scenario.name) {
                    continue;
                }
            }

            futures.push(self.run_scenario(
                &feature,
                rule,
                &scenario,
                &before_fns,
                &after_fns,
                options.suppress_output,
                output.clone(),
            ))
        }

        // Check if all are successful
        futures::future::join_all(futures)
            .await
            .into_iter()
            .all(|x| x)
    }

    pub async fn run(
        &self,
        feature_files: Vec<PathBuf>,
        before_fns: Option<&[HelperFn]>,
        after_fns: Option<&[HelperFn]>,
        options: cli::CliOptions,
        output: &mut impl OutputVisitor,
    ) -> bool {
        output.visit_start();

        let mut is_success = true;

        for path in feature_files {
            let mut file = File::open(&path).expect("file to open");
            let mut buffer = String::new();
            file.read_to_string(&mut buffer).unwrap();

            let feature = match Feature::try_from(&*buffer) {
                Ok(v) => v,
                Err(e) => {
                    output.visit_feature_error(&path, &e);
                    is_success = false;
                    continue;
                }
            };

            output.visit_feature(&feature, &path);
            if !self
                .run_scenarios(
                    &feature,
                    None,
                    &feature.scenarios,
                    before_fns,
                    after_fns,
                    &options,
                    output,
                )
                .await
            {
                is_success = false;
            }

            for rule in &feature.rules {
                output.visit_rule(&rule);
                if !self
                    .run_scenarios(
                        &feature,
                        Some(&rule),
                        &rule.scenarios,
                        before_fns,
                        after_fns,
                        &options,
                        output,
                    )
                    .await
                {
                    is_success = false;
                }
                output.visit_rule_end(&rule);
            }
            output.visit_feature_end(&feature);
        }

        output.visit_finish();

        is_success
    }
}

#[doc(hidden)]
pub fn tag_rule_applies(scenario: &Scenario, rule: &str) -> bool {
    if let Some(ref tags) = &scenario.tags {
        let tags: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
        let rule_chunks = rule.split(' ');
        // TODO: implement a sane parser for this
        for rule in rule_chunks {
            if rule == "and" || rule == "or" {
                // TODO: implement handling for this
                continue;
            }

            if !tags.contains(&rule) {
                return false;
            }
        }

        true
    } else {
        true
    }
}

#[macro_export]
macro_rules! before {
    (
        $fnname:ident: $tagrule:tt => $scenariofn:expr
    ) => {
        fn $fnname(scenario: &$crate::Scenario) {
            let scenario_closure: fn(&$crate::Scenario) -> () = $scenariofn;
            let tag_rule: &str = $tagrule;

            // TODO check tags
            if $crate::tag_rule_applies(scenario, tag_rule) {
                scenario_closure(scenario);
            }
        }
    };

    (
        $fnname:ident => $scenariofn:expr
    ) => {
        before!($fnname: "" => $scenariofn);
    };
}

// This is just a remap of before.
#[macro_export]
macro_rules! after {
    (
        $fnname:ident: $tagrule:tt => $stepfn:expr
    ) => {
        before!($fnname: $tagrule => $stepfn);
    };

    (
        $fnname:ident => $scenariofn:expr
    ) => {
        before!($fnname: "" => $scenariofn);
    };
}

#[macro_export]
macro_rules! cucumber {
    (
        features: $featurepath:tt,
        world: $worldtype:path,
        steps: $vec:expr,
        setup: $setupfn:expr,
        before: $beforefns:expr,
        after: $afterfns:expr
    ) => {
        cucumber!(@finish; $featurepath; $worldtype; $vec; Some($setupfn); Some($beforefns); Some($afterfns));
    };

    (
        features: $featurepath:tt,
        world: $worldtype:path,
        steps: $vec:expr,
        setup: $setupfn:expr,
        before: $beforefns:expr
    ) => {
        cucumber!(@finish; $featurepath; $worldtype; $vec; Some($setupfn); Some($beforefns); None);
    };

        (
        features: $featurepath:tt,
        world: $worldtype:path,
        steps: $vec:expr,
        setup: $setupfn:expr,
        after: $afterfns:expr
    ) => {
        cucumber!(@finish; $featurepath; $worldtype; $vec; Some($setupfn); None; Some($afterfns));
    };

    (
        features: $featurepath:tt,
        world: $worldtype:path,
        steps: $vec:expr,
        before: $beforefns:expr,
        after: $afterfns:expr
    ) => {
        cucumber!(@finish; $featurepath; $worldtype; $vec; None; Some($beforefns); Some($afterfns));
    };

    (
        features: $featurepath:tt,
        world: $worldtype:path,
        steps: $vec:expr,
        before: $beforefns:expr
    ) => {
        cucumber!(@finish; $featurepath; $worldtype; $vec; None; Some($beforefns); None);
    };

    (
        features: $featurepath:tt,
        world: $worldtype:path,
        steps: $vec:expr,
        after: $afterfns:expr
    ) => {
        cucumber!(@finish; $featurepath; $worldtype; $vec; None; None; Some($afterfns));
    };

    (
        features: $featurepath:tt,
        world: $worldtype:path,
        steps: $vec:expr,
        setup: $setupfn:expr
    ) => {
        cucumber!(@finish; $featurepath; $worldtype; $vec; Some($setupfn); None; None);
    };

    (
        features: $featurepath:tt,
        world: $worldtype:path,
        steps: $vec:expr
    ) => {
        cucumber!(@finish; $featurepath; $worldtype; $vec; None; None; None);
    };

    (
        @finish; $featurepath:tt; $worldtype:path; $vec:expr; $setupfn:expr; $beforefns:expr; $afterfns:expr
    ) => {
        #[allow(unused_imports)]
        #[runtime::main]
        async fn main() {
            use std::path::Path;
            use std::process;
            use $crate::globwalk::{glob, GlobWalkerBuilder};
            use $crate::gherkin::Scenario;
            use $crate::{Steps, World, DefaultOutput};
            use $crate::cli::make_app;

            let options = make_app().unwrap();

            let walker = match &options.feature {
                Some(v) => glob(v).expect("feature glob is invalid"),
                None => match Path::new($featurepath).canonicalize() {
                    Ok(p) => {
                        GlobWalkerBuilder::new(p, "*.feature")
                            .case_insensitive(true)
                            .build()
                            .expect("feature path is invalid")
                    }
                    Err(e) => {
                        eprintln!("{}", e);
                        eprintln!("There was an error parsing \"{}\"; aborting.", $featurepath);
                        process::exit(1);
                    }
                }
            }.into_iter();

            let mut feature_files = walker
                .filter_map(Result::ok)
                .map(|entry| entry.path().to_owned())
                .collect::<Vec<_>>();
            feature_files.sort();

            let tests = Steps::combine($vec.iter().map(|f| f()));

            let mut output = DefaultOutput::default();

            let setup_fn: Option<fn() -> ()> = $setupfn;
            let before_fns: Option<&[fn(&Scenario) -> ()]> = $beforefns;
            let after_fns: Option<&[fn(&Scenario) -> ()]> = $afterfns;

            if let Some(setup_fn) = setup_fn {
                setup_fn();
            }

            if !tests.run(feature_files, before_fns, after_fns, options, &mut output).await {
                process::exit(1);
            }
        }
    }
}

#[macro_export]
macro_rules! typed_regex {
    (
        $worldtype:path, ($($arg_type:ty),*) $body:expr
    ) => {
        |world: &mut $worldtype, matches, step| {
            let body: fn(&mut $worldtype, $($arg_type,)* &$crate::Step) -> () = $body;
            let mut matches = matches.into_iter().enumerate().skip(1);

            body(
                world,
                $({
                    let (index, match_) = matches.next().unwrap();
                    match_.parse::<$arg_type>().unwrap_or_else(|_| panic!("Failed to parse argument {} with value '{}' to type {}", index, match_, stringify!($arg_type)))
                },)*
                step
            )
        }
    };
}

#[macro_export]
macro_rules! skip {
    () => {
        unimplemented!("cucumber test skipped");
    };
}

#[macro_export]
macro_rules! steps {
    (
        @step_type given
    ) => {
        $crate::StepType::Given
    };

    (
        @step_type when
    ) => {
        $crate::StepType::When
    };

    (
        @step_type then
    ) => {
        $crate::StepType::Then
    };

    (
        @parse_matches $worldtype:path, ($($arg_type:ty),*) $body:expr
    ) => {
        $crate::typed_regex!($worldtype, ($($arg_type),*) $body)
    };

    (
        @gather_steps, $worldtype:path, $tests:tt,
        $ty:ident regex $name:tt $body:expr;
    ) => {
        $tests.add_regex(steps!(@step_type $ty), $name, $body);
    };

    (
        @gather_steps, $worldtype:path, $tests:tt,
        $ty:ident regex $name:tt $body:expr; $( $items:tt )*
    ) => {
        $tests.add_regex(steps!(@step_type $ty), $name, $body);

        steps!(@gather_steps, $worldtype, $tests, $( $items )*);
    };

    (
        @gather_steps, $worldtype:path, $tests:tt,
        $ty:ident regex $name:tt ($($arg_type:ty),*) $body:expr;
    ) => {
        steps!(@gather_steps, $worldtype, $tests, $ty regex $name steps!(@parse_matches $worldtype, ($($arg_type),*) $body););
    };

    (
        @gather_steps, $worldtype:path, $tests:tt,
        $ty:ident regex $name:tt ($($arg_type:ty),*) $body:expr; $( $items:tt )*
    ) => {
        steps!(@gather_steps, $worldtype, $tests, $ty regex $name steps!(@parse_matches $worldtype, ($($arg_type),*) $body); $( $items )*);
    };

    (
        @gather_steps, $worldtype:path, $tests:tt,
        $ty:ident $name:tt $body:expr;
    ) => {
        $tests.add_normal(steps!(@step_type $ty), $name, $body);
    };

    (
        @gather_steps, $worldtype:path, $tests:tt,
        $ty:ident $name:tt $body:expr; $( $items:tt )*
    ) => {
        $tests.add_normal(steps!(@step_type $ty), $name, $body);

        steps!(@gather_steps, $worldtype, $tests, $( $items )*);
    };

    (
        $worldtype:path => { $( $items:tt )* }
    ) => {
        pub fn steps() -> $crate::Steps<$worldtype> {
            let mut tests: $crate::StepsBuilder::<$worldtype> = $crate::StepsBuilder::new();
            steps!(@gather_steps, $worldtype, tests, $( $items )*);
            tests.build()
        }
    };
}
