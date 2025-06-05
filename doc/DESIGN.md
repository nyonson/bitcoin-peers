# Design

Historical lessons and findings along with remaining open questions.

## Connection

At some point it will probably make sense to have an event loop in the `Connection` struct that handles certain interactions automatically, like the ping/pongs. `Connection` is currently still used as a short-lived interaction, but if this changes, that is probably when it should be improved. Perhaps both scenarios can be well supported by adding some sort of "listen" method which fires up an event loop and tosses back requested message types over a channel.

## Crawler

The crawler has the potential to fire up way too many tasks on a system as it bounces across the network. It would be neat if there was some sort of backpressure approach to control the resource usage. But what we probably (not verified by lookin at actual resource usage) want to control is the number of task, which isn't directly connected to the number of "discovered" peers queue'd up. A single task can dump 1000 peers in that bucket, or none.

## Peer Quality

What are heuristics for measuring the quality of a peer?
