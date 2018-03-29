#!/bin/bash
let i=1
while [ $i -le 16 ]
do
time cargo run --release -- -d 0 -w 16 4445 "[fe80::932d:3fe2:7670:135d]:4444" "http://i0.kym-cdn.com/entries/icons/original/000/019/472/i09FJm4.jpg"
let i++
done
