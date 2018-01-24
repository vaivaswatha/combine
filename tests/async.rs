#[macro_use]
extern crate quickcheck;

extern crate bytes;
#[macro_use]
extern crate combine;

extern crate futures;
extern crate partial_io;
extern crate tokio_io;

use std::any::Any;
use std::cell::Cell;
use std::str;
use std::io::Cursor;
use std::rc::Rc;

use bytes::BytesMut;

use futures::{Future, Stream};
use tokio_io::codec::FramedRead;

use tokio_io::codec::Decoder;

use combine::range::{range, recognize_with_value, take_while, take_while1};
use combine::{any, count_min_max, skip_many, Parser, many1};
use combine::combinator::{boxed_partial_state, no_partial, optional, recognize, skip_many1};
use combine::primitives::RangeStream;
use combine::easy;
use combine::char::{char, digit, letter};

macro_rules! mk_parser {
    ($parser: expr, $self_: expr, ()) =>  { $parser };
    ($parser: expr, $self_: expr, ($custom_state: ty)) =>  { $parser($self_.1.clone()) };
}
macro_rules! impl_decoder {
    ($typ: ident, $item: ty, $parser: expr, $custom_state: ty) => {
        #[derive(Default)]
        struct $typ(Option<Box<Any>>, $custom_state);
        impl_decoder!{$typ, $item, $parser; ($custom_state)}
    };
    ($typ: ident, $item: ty, $parser: expr) => {
        #[derive(Default)]
        struct $typ(Option<Box<Any>>);
        impl_decoder!{$typ, $item, $parser; ()}
    };
    ($typ: ident, $item: ty, $parser: expr; ( $($custom_state: tt)* )) => {
        impl Decoder for $typ {
            type Item = $item;
            type Error = Box<::std::error::Error + Send + Sync>;

            fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
                (&mut &mut *self).decode(src)
            }
        }

        impl<'a> Decoder for &'a mut $typ {
            type Item = $item;
            type Error = Box<::std::error::Error + Send + Sync>;

            fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
                let (opt, removed_len) = {
                    let str_src = str::from_utf8(&src[..])?;
                    println!("Decoding `{}`", str_src);
                    combine::async::decode(
                        mk_parser!($parser, self, ($($custom_state)*)),
                        easy::Stream(combine::primitives::PartialStream(str_src)),
                        &mut self.0,
                    ).map_err(|err| {
                        // Since err contains references into `src` we must remove these before
                        // returning the error and before we call `split_to` to remove the input we
                        // just consumed
                        let err = err.map_range(|r| r.to_string())
                            .map_position(|p| p.translate_position(&src[..]));
                        format!("{}\nIn input: `{}`", err, str_src)
                    })?
                };

                src.split_to(removed_len);
                match opt {
                    None => println!("Need more input!"),
                    Some(_) => (),
                }
                Ok(opt)
            }
        }
    }
}

use partial_io::{GenWouldBlock, PartialAsyncRead, PartialOp, PartialWithErrors};

fn run_decoder<D, S>(input: &str, seq: S, decoder: D) -> Result<Vec<D::Item>, D::Error>
where
    D: Decoder,
    D::Item: ::std::fmt::Display,
    S: IntoIterator<Item = PartialOp> + 'static,
    S::IntoIter: Send,
{
    let ref mut reader = Cursor::new(input.as_bytes());
    let partial_reader = PartialAsyncRead::new(reader, seq);
    FramedRead::new(partial_reader, decoder)
        .map(|x| {
            println!("Decoded `{}`", x);
            x
        })
        .collect()
        .wait()
}

parser!{
    type PartialState = Option<Box<Any>>;
    fn basic_parser['a, I]()(I) -> String
        where [ I: RangeStream<Item = char, Range = &'a str> ]
    {
        many1(digit()).skip(range(&"\r\n"[..]))
    }
}

impl_decoder!{ Basic, String, basic_parser() }

#[test]
fn many1_skip_no_errors() {
    let input = "123\r\n\
                 456\r\n";

    let result = run_decoder(input, vec![], Basic::default());

    assert!(result.as_ref().is_ok(), "{}", result.unwrap_err());
    assert_eq!(result.unwrap(), vec!["123".to_string(), "456".to_string()]);
}

parser!{
    type PartialState = Option<Box<Any>>;
    fn prefix_many_then_parser['a, I]()(I) -> String
        where [ I: RangeStream<Item = char, Range = &'a str> ]
    {
        (char('#'), skip_many(char(' ')), many1(digit()))
            .then_partial(|t| {
                let s: &String = &t.2;
                let c = s.parse().unwrap();
                count_min_max(c, c, any())
            })
    }
}

parser!{
    type PartialState = Option<Box<Any>>;
    fn choice_parser['a, I]()(I) -> String
        where [ I: RangeStream<Item = char, Range = &'a str> ]
    {
        many1(digit())
            .or(many1(letter()))
            .skip(range(&"\r\n"[..]))
    }
}

quickcheck! {
    fn many1_skip_test(seq: PartialWithErrors<GenWouldBlock>) -> () {

        let input = "123\r\n\
                     456\r\n\
                     1\r\n\
                     5\r\n\
                     666666\r\n";

        let result = run_decoder(input, seq, Basic::default());

        assert!(result.as_ref().is_ok(), "{}", result.unwrap_err());
        assert_eq!(
            result.unwrap(),
            vec!["123".to_string(), "456".to_string(), "1".to_string(), "5".to_string(), "666666".to_string()]
        );
    }

    fn prefix_many_then_test(seq: PartialWithErrors<GenWouldBlock>) -> () {
        impl_decoder!{ TestParser, String, prefix_many_then_parser() }

        let input = "# 1a\
                     # 4abcd\
                     #0\
                     #3:?a\
                     #10abcdefghij";

        let result = run_decoder(input, seq, TestParser::default());

        assert!(result.as_ref().is_ok(), "{}", result.unwrap_err());
        assert_eq!(
            result.unwrap(),
            ["a", "abcd", "", ":?a", "abcdefghij"],
        );
    }

    fn choice_test(seq: PartialWithErrors<GenWouldBlock>) -> () {
        impl_decoder!{ TestParser, String, choice_parser() }

        let input = "1\r\n\
                     abcd\r\n\
                     123\r\n\
                     abc\r\n\
                     1232751\r\n";

        let result = run_decoder(input, seq, TestParser::default());

        assert!(result.as_ref().is_ok(), "{}", result.unwrap_err());
        assert_eq!(
            result.unwrap(),
            ["1", "abcd", "123", "abc", "1232751"],
        );
    }

    fn recognize_test(seq: PartialWithErrors<GenWouldBlock>) -> () {
        impl_decoder!{ TestParser, String,
            boxed_partial_state(
                recognize(
                    (skip_many1(digit()), optional((char('.'), skip_many(digit()))))
                )
                .skip(range(&"\r\n"[..]))
            )
        }

        let input = "1.0\r\n\
                     123.123\r\n\
                     17824\r\n\
                     3.14\r\n\
                     1.\r\n\
                     2\r\n";

        let result = run_decoder(input, seq, TestParser::default());

        assert!(result.as_ref().is_ok(), "{}", result.unwrap_err());
        assert_eq!(
            result.unwrap(),
            ["1.0", "123.123", "17824", "3.14", "1.", "2"],
        );
    }

    fn recognize_range_test(seq: PartialWithErrors<GenWouldBlock>) -> () {
        impl_decoder!{ TestParser, String,
            boxed_partial_state(
                recognize_with_value(
                    (skip_many1(digit()), optional((char('.'), skip_many(digit()))))
                )
                .map(|(r, _)| String::from(r))
                .skip(range(&"\r\n"[..]))
            )
        }

        let input = "1.0\r\n\
                     123.123\r\n\
                     17824\r\n\
                     3.14\r\n\
                     1.\r\n\
                     2\r\n";

        let result = run_decoder(input, seq, TestParser::default());

        assert!(result.as_ref().is_ok(), "{}", result.unwrap_err());
        assert_eq!(
            result.unwrap(),
            ["1.0", "123.123", "17824", "3.14", "1.", "2"],
        );
    }

    fn take_while_test(seq: PartialWithErrors<GenWouldBlock>) -> () {
        impl_decoder!{ TestParser, String,
            |counter: Rc<Cell<i32>>| boxed_partial_state(
                take_while(move |c| { counter.set(counter.get() + 1); c != '\r' })
                    .map(String::from)
                    .skip(range("\r\n"))
            ),
            Rc<Cell<i32>>
        }

        let input = "1.0\r\n\
                     123.123\r\n\
                     17824\r\n\
                     3.14\r\n\
                     \r\n\
                     2\r\n";

        let counter = Rc::new(Cell::new(0));
        let result = run_decoder(input, seq, TestParser(Default::default(), counter.clone()));

        assert!(result.as_ref().is_ok(), "{}", result.unwrap_err());
        assert_eq!(
            result.unwrap(),
            ["1.0", "123.123", "17824", "3.14", "", "2"],
        );

        assert_eq!(counter.get(), 26);
    }

    fn take_while1_test(seq: PartialWithErrors<GenWouldBlock>) -> () {
        impl_decoder!{ TestParser, String,
            |count: Rc<Cell<i32>>| boxed_partial_state(
                take_while1(move |c| { count.set(count.get() + 1); c != '\r' })
                    .map(String::from)
                    .skip(range("\r\n"))
            ),
            Rc<Cell<i32>>
        }

        let input = "1.0\r\n\
                     123.123\r\n\
                     17824\r\n\
                     3.14\r\n\
                     1.\r\n\
                     2\r\n";

        let counter = Rc::new(Cell::new(0));
        let result = run_decoder(input, seq, TestParser(Default::default(), counter.clone()));

        assert!(result.as_ref().is_ok(), "{}", result.unwrap_err());
        assert_eq!(
            result.unwrap(),
            ["1.0", "123.123", "17824", "3.14", "1.", "2"],
        );

        assert_eq!(counter.get(), 28);
    }
}
#[test]
fn inner_no_partial_test() {
    let seq = vec![PartialOp::Limited(10)];
    impl_decoder!{ TestParser, String,
        boxed_partial_state(
            no_partial(many1(digit()))
                .or(many1(letter()))
                .skip(range(&"\r\n"[..]))
        )
    }

    let input = "1\r\n\
                 abcd\r\n\
                 123\r\n\
                 abc\r\n\
                 1232751\r\n";

    let result = run_decoder(input, seq, TestParser::default());

    assert!(result.as_ref().is_ok(), "{}", result.unwrap_err());
    assert_eq!(result.unwrap(), ["1", "abcd", "123", "abc", "1232751"],);
}
