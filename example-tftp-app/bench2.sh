#!/bin/bash
let i=1
while [ $i -le 16 ]
do
time cargo run --release -- -d 0 -w 1 "0.0.0.0:5445" "129.3.20.66:5444" "http://i0.kym-cdn.com/entries/icons/original/000/019/472/i09FJm4.jpg"
let i++
done

echo "real 1000.00s"

let i=1
while [ $i -le 16 ]
do
time cargo run --release -- -d 1 -w 1 "0.0.0.0:5445" "129.3.20.66:5444" "http://i0.kym-cdn.com/entries/icons/original/000/019/472/i09FJm4.jpg"
let i++
done

echo "real 1000.00s"

let i=1
while [ $i -le 16 ]
do
time cargo run --release -- -d 13 -w 1 "0.0.0.0:5445" "129.3.20.66:5444" "http://i0.kym-cdn.com/entries/icons/original/000/019/472/i09FJm4.jpg"
let i++
done
