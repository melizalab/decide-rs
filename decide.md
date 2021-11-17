
This document specifies the protocols used by the *decide* operant control
software to manipulate experimental state and broadcast state changes.

-   Name: 3/decide
-   Editor: Dan Meliza (dan at meliza.org)
-   Version: 2.0
-   State:  draft
-   URL: <https://meliza.org/spec:3/decide-ctrl/>

## Goals and framework

Automated operant behavioral experiments involve the following processes:

1. Physical control of operant apparatus, including sound playback for stimulus
   presentation.
2. Sequencing and control of experimental trials. For example, a typical trial
   comprises presentation of one or more stimuli, detection of correct and
   incorrect responses, and provision of positive and negative feedback. In
   addition, trials may be structured into blocks with varying experimental
   conditions.
3. Logging of apparatus state changes.
4. Interfaces to monitor and manipulate the apparatus and experimental progress,
   including starting and stopping experiments.

In *decide*, these processes are implemented in programs that may be distributed
over multiple computers. The apparatus control processes run on small embedded
computers that interface directly with operant hardware through general-purpose
input-output (GPIO) lines or local buses like I2C. Each of these hardware `components` has a unique identifier.

This protocol defines how an experimental control programs (`client`)
communicates with an apparatus control process (`controller`). There are two
kinds of information flow.

To request information about the capabilities and configuration of the
`controller` or changes to the state of a `component`, the `client` sends a
message through a synchronous request-reply (REQ-REP) channel. The `controller`
replies to each request with an acknowledgement, the requested information, or
with an error message.

When the state of a `component` changes, either as the result of a request or an
action by the experimental subject, the `controller` sends this information to the
`client` through an asynchronous PUB-SUB channel.

## Messages

The protocols described here are intended to be as independent of the wire
protocol as possible, but the current implementation uses
[zeromq](https://zeromq.org), with an asynchronous PUB channel and a synchronous
REQ channel.

TODO: define port numbers here. Current temporary ports:

- request port 7897
- publish port 7898

### PUB channel

PUB messages are sent asynchronously and do not require a response. In zeromq, PUB messages have *topics*, and subscribers can specify which messages to receive based on `topic`. In this protocol, messages are given the following PUB topics:

#### State changes

Changes to the state of a component are published under the topic `state/name`, where `name` is the name of the component. All components have unique names. The payload of the message comprises a [protocol buffer](https://developers.google.com/protocol-buffers/) with the following specification:

``` protocol-buffer
message StateChange {
  // when the state change occurred, in microseconds since the epoch
  required uint64 time = 1;
}
```

TODO: Can we nail down the spec for the values of the state? The data type will vary depending on the component.

#### Log messages

Operational messages are published under the topic `log/level`, where `level` is one of the following values: `error`, `warning`, `info`, or `debug`. The payload of the message must comprise a UTF-8 encoded string with the cause of the logging event.

### REQ messages

REQ messages use a synchronous request-reply pattern. The client initiates each exchange and must not send an additional request until it receives a reply.

A request consists of the following zmq frames:

- Frame 0: Empty (zero bytes, invisible to REQ application)
- Frame 1: "DCDC01" (six bytes, representing decide/control v0.1)
- Frame 2: Request type (one byte, see below)
- Frame 3: Request body (message type dependent)

#### Change state (0x00)

Requests a modification to the state of the component specified by `name`. Controller will reply with error (0x01) if the component does not exist or the request was badly formed, and with OK (0x00) otherwise. Note that the actual state change will be broadcast on the PUB channel.

#### Reset state (0x01)

Requests that the state of the component specified by `name` be reset to its default value. Controller will reply with error (0x01) if the component does not exist or the request was badly formed, and with OK (0x00) otherwise. Note that the actual state change will be broadcast on the PUB channel.

#### Set parameters (0x02)

#### Get component parameters (0x12)

#### Get component state type (0x13)

#### Lock experiment (0x20)

#### Unlock experiment (0x21)

### REP messages

For each REQ message, the `controller` must respond with a REP consisting of the following zmq frames:

- Frame 0: Empty (zero bytes, invisible to REQ application)
- Frame 1: "DCDC01" (six bytes, representing decide/control v0.1)
- Frame 2: Reply type (one byte, see below)
- Frame 3: Reply body (message type dependent)

#### OK (0x00)

#### error (0x01)

#### Get component parameters (0x12)

``` protocol-buffer

```

#### Get component state type (0x13)

``` protocol-buffer

```
