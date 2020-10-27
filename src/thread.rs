// Copyright 2020 Chaos Mesh Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// See the License for the specific language governing permissions and
// limitations under the License.

use std::sync::mpsc::{channel, Receiver};

pub struct JoinHandle<T> {
    channel: Receiver<T>,
}

impl<T> JoinHandle<T>
where
    T: Send + 'static,
{
    pub fn join(self) -> T {
        self.channel.recv().unwrap()
    }
}

pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        let result = f();

        sender.send(result).unwrap();

        std::process::exit(0);
    });

    return JoinHandle::<T> { channel: receiver };
}
