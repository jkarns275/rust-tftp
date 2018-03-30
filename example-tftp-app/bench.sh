#!/bin/bash
let i=1
while [ $i -le 16 ]
do
time cargo run --release -- -d 0 -w 16 "[::0]:4445" "[fe80::1d8e:dd4b:dc6f:d375]:4444" "http://i0.kym-cdn.com/entries/icons/original/000/019/472/i09FJm4.jpg"
let i++
done

echo "real 1000.00s"

let i=1
while [ $i -le 16 ]
do
time cargo run --release -- -d 1 -w 16 "[::0]:4445" "[fe80::1d8e:dd4b:dc6f:d375]:4444" "http://i0.kym-cdn.com/entries/icons/original/000/019/472/i09FJm4.jpg"
let i++
done

echo "real 1000.00s"

let i=1
while [ $i -le 16 ]
do
time cargo run --release -- -d 13 -w 16 "[::0]:4445" "[fe80::1d8e:dd4b:dc6f:d375]:4444" "http://i0.kym-cdn.com/entries/icons/original/000/019/472/i09FJm4.jpg"
let i++
done
